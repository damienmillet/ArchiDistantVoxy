//! On-demand distant terrain generation for LOD sections (EF-3, ENF-2).
//!
//! Generates chunks with pumpkin-world's `generate_single_chunk` (outside the
//! loaded area, deterministic from the seed), then samples them into a LOD
//! section through the shared `TerrainCapture`. Work runs on dedicated Tokio
//! blocking threads so the server tick is never blocked.
//! SPDX-License-Identifier: GPL-3.0-or-later.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::format_collect,
    clippy::needless_pass_by_value
)]

use std::sync::Arc;

use pumpkin::server::Server;
use pumpkin::world::World;
use pumpkin_world::chunk_system::generation::generate_single_chunk;
use pumpkin_world::chunk_system::{Chunk, StagedChunkEnum};
use pumpkin_world::generation::proto_chunk::ProtoChunk;
use lod_core::builder::{encode_section, CapturedSection};
use lod_core::capture::{
    CaptureError as TerrainError, CaptureOptions, TerrainCapture, TerrainRegion, TerrainSample,
    TerrainSource,
};
use lod_core::message::Section;

/// Number of blocking generation threads (ENF-2 CPU budget).
pub const GENERATION_THREADS: usize = 2;

/// Terrain source over the four generated chunks of one LOD section
/// (64x64 blocks = 2x2 Pumpkin chunks).
pub struct ChunkedTerrain<'a> {
    protos: [&'a ProtoChunk; 4],
    min_y: i32,
    height: i32,
}

impl<'a> ChunkedTerrain<'a> {
    /// Builds a terrain source over the four chunks of a LOD section, in
    /// quadrant order: north-west, north-east, south-west, south-east.
    #[must_use]
    pub fn new(
        protos: [&'a ProtoChunk; 4],
        min_y: i32,
        height: i32,
    ) -> Self {
        Self {
            protos,
            min_y,
            height,
        }
    }

}

/// Maps an absolute block position to (chunk quadrant, chunk-local x, chunk-local z)
/// inside one 64x64 LOD section made of 2x2 chunks.
fn locate(x: i32, z: i32) -> (usize, i32, i32) {
    let section_local_x = x & 63;
    let section_local_z = z & 63;
    let quadrant = usize::from(section_local_x >= 16) + 2 * usize::from(section_local_z >= 16);
    (quadrant, section_local_x & 15, section_local_z & 15)
}

impl TerrainSource for ChunkedTerrain<'_> {
    fn height(&self) -> i32 {
        self.height
    }

    fn min_y(&self) -> i32 {
        self.min_y
    }

    fn sample(&self, x: i32, y: i32, z: i32) -> Result<TerrainSample, String> {
        let (quadrant, _local_x, _local_z) = locate(x, z);
        let proto = self.protos[quadrant];
        let pos = pumpkin_util::math::vector3::Vector3::new(x, y, z);
        let state_id = proto.get_block_state(&pos);
        let block = state_id.to_block();
        let biome = proto.get_biome(x, y, z);

        let mut material = format!("{}_DH-BSW_minecraft:{}", biome.registry_id, block.name);
        if let Some(properties) = block.properties(state_id) {
            let mut sorted: Vec<(&'static str, &'static str)> = properties
                .to_props()
                .into_iter()
                .map(|property| (property.key().to_str(), property.value_str()))
                .collect();
            sorted.sort_unstable_by_key(|(name, _)| *name);
            let mut joined = String::new();
            for (name, value) in sorted {
                use std::fmt::Write as _;
                let _ = write!(joined, "{{{{{name}:{value}}}}}");
            }
            if !joined.is_empty() {
                material.push_str("_STATE_");
                material.push_str(&joined);
            }
        }

        Ok(TerrainSample {
            material,
            sky_light: 15,
            block_light: state_id.to_state().luminance,
        })
    }

    fn top(&self, x: i32, z: i32) -> Result<i32, String> {
        let (quadrant, local_x, local_z) = locate(x, z);
        let proto = self.protos[quadrant];
        let top = proto.flat_surface_height_map[usize::try_from(local_z * 16 + local_x).unwrap_or(0)];
        Ok(i32::from(top))
    }
}

/// Errors raised while generating a LOD section.
#[derive(Debug)]
pub enum GenerationError {
    /// The world or its generator was unavailable.
    Unavailable(String),
    /// Terrain sampling failed.
    Terrain(TerrainError),
    /// The encoded section was rejected by the DTO encoder.
    Encode(String),
}

impl std::fmt::Display for GenerationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(what) => write!(f, "generation unavailable: {what}"),
            Self::Terrain(e) => write!(f, "terrain capture failed: {e}"),
            Self::Encode(e) => write!(f, "section encoding failed: {e}"),
        }
    }
}

impl std::error::Error for GenerationError {}

/// Generates the four chunks of one LOD section and captures it into an
/// encoded DH section. Runs on a blocking thread; never call from the tick.
///
/// # Errors
/// Fails when the world generator is unavailable, generation diverges from the
/// expected stage, or the capture fails.
pub fn generate_section(
    server: &Arc<Server>,
    world: &Arc<World>,
    section: &Section,
    now: u64,
) -> Result<Vec<u8>, GenerationError> {
    let _ = server;
    let level = &world.level;
    let generator = level.world_gen.load_full();
    let Some(portal) = level.world_portal.load_full() else {
        return Err(GenerationError::Unavailable(
            "world portal not set".to_string(),
        ));
    };

    let section_blocks = lod_core::SECTION_SIZE_BLOCKS as i32;
    let chunk_x = section.x * (section_blocks / 16);
    let chunk_z = section.z * (section_blocks / 16);

    let coords = [
        (chunk_x, chunk_z),
        (chunk_x + 1, chunk_z),
        (chunk_x, chunk_z + 1),
        (chunk_x + 1, chunk_z + 1),
    ];
    let mut owned: Vec<Box<ProtoChunk>> = Vec::with_capacity(4);
    for (cx, cz) in coords {
        match generate_single_chunk(
            &generator,
            portal.as_ref().as_ref(),
            cx,
            cz,
            StagedChunkEnum::Features,
        ) {
            Chunk::Proto(proto) => owned.push(proto),
            Chunk::Level(_) => {
                return Err(GenerationError::Unavailable(format!(
                    "chunk ({cx}, {cz}) reached an unexpected stage"
                )));
            }
        }
    }
    let protos: [&ProtoChunk; 4] = [
        owned[0].as_ref(),
        owned[1].as_ref(),
        owned[2].as_ref(),
        owned[3].as_ref(),
    ];

    let min_y = world.dimension.min_y;
    let height = world.dimension.height as i32;
    let terrain = ChunkedTerrain::new(protos, min_y, height);
    let region = TerrainRegion {
        width: lod_core::SECTION_SIZE_BLOCKS as usize,
        depth: lod_core::SECTION_SIZE_BLOCKS as usize,
        height: usize::try_from(height).unwrap_or(384),
        min_y,
        origin_x: chunk_x * 16,
        origin_z: chunk_z * 16,
    };
    let mut capture = TerrainCapture::new(region, CaptureOptions::dh());
    loop {
        if capture
            .step(&terrain, 4096)
            .map_err(GenerationError::Terrain)?
        {
            break;
        }
    }
    let captured: CapturedSection = capture.result().map_err(GenerationError::Terrain)?;
    encode_section(section, &captured, now).map_err(|e| GenerationError::Encode(e.to_string()))
}
