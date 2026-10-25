//! Incremental terrain capture for LOD sections.
//!
//! Samples a rectangular region column by column (top to bottom), grouping
//! consecutive identical samples into vertical segments under a bounded sample
//! budget. Ported from the pumpkin-plugins terrain capture. SPDX-License-
//! Identifier: GPL-3.0-or-later.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss
)]

use std::collections::HashMap;

use crate::builder::{CapturedSection, Column, Segment};

/// A rectangular area expressed in absolute block coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerrainRegion {
    /// Region width in blocks (X axis).
    pub width: usize,
    /// Region depth in blocks (Z axis).
    pub depth: usize,
    /// Region height in blocks.
    pub height: usize,
    /// Absolute minimum Y of the region.
    pub min_y: i32,
    /// Absolute origin X (block coordinate).
    pub origin_x: i32,
    /// Absolute origin Z (block coordinate).
    pub origin_z: i32,
}

impl TerrainRegion {
    /// Number of columns in the region.
    #[must_use]
    pub fn area(&self) -> usize {
        self.width * self.depth
    }
}

/// A sampled material and its light values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerrainSample {
    /// DH material name.
    pub material: String,
    /// Sky light (0-15).
    pub sky_light: u8,
    /// Block light (0-15).
    pub block_light: u8,
}

/// Terrain values exposed by a host adapter during one capture step.
pub trait TerrainSource {
    /// World height as seen by this source.
    fn height(&self) -> i32;
    /// World minimum Y as seen by this source.
    fn min_y(&self) -> i32;
    /// Samples one block.
    ///
    /// # Errors
    /// Fails when the block cannot be read.
    fn sample(&self, x: i32, y: i32, z: i32) -> Result<TerrainSample, String>;
    /// Inclusive world Y of the highest non-empty block in a column.
    ///
    /// # Errors
    /// Fails when the column cannot be read.
    fn top(&self, x: i32, z: i32) -> Result<i32, String>;
}

/// Options controlling material grouping and the retained segment limit.
#[derive(Clone, Copy)]
pub struct CaptureOptions<'a> {
    /// Identifies empty material values so air above a height bound can be skipped.
    pub is_empty: &'a dyn Fn(&str) -> bool,
    /// Maximum number of segments retained across the full region.
    pub max_segments: usize,
}

impl CaptureOptions<'_> {
    /// Default options: DH empty materials, [`crate::builder::MAX_POINTS_PER_SECTION`] segments.
    #[must_use]
    pub fn dh() -> Self {
        Self {
            is_empty: &|material: &str| {
                material.ends_with("_DH-BSW_minecraft:air")
                    || material.ends_with("_DH-BSW_minecraft:void_air")
            },
            max_segments: crate::builder::MAX_POINTS_PER_SECTION,
        }
    }
}

/// Capture error variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureError {
    /// The region or budget is invalid.
    Invalid(&'static str),
    /// The source height or minimum Y changed mid-capture.
    WorldHeightChanged,
    /// The segment budget was exceeded.
    TooComplex,
    /// The source failed to sample.
    Source(String),
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(what) => write!(f, "invalid capture {what}"),
            Self::WorldHeightChanged => write!(f, "world height changed mid-capture"),
            Self::TooComplex => write!(f, "terrain region is too complex"),
            Self::Source(e) => write!(f, "terrain source failed: {e}"),
        }
    }
}

impl std::error::Error for CaptureError {}

/// Builds bounded column data incrementally from a host-provided terrain source.
pub struct TerrainCapture<'a> {
    region: TerrainRegion,
    is_empty: &'a dyn Fn(&str) -> bool,
    max_segments: usize,
    columns: Vec<Column>,
    material_ids: HashMap<String, u32>,
    materials: Vec<String>,
    column: usize,
    y: i32,
    segment_count: usize,
    sampled: u64,
}

impl<'a> TerrainCapture<'a> {
    /// Initializes an empty capture starting at the highest Y of the first column.
    ///
    /// # Errors
    /// Fails when the region or segment limit is invalid.
    #[must_use]
    pub fn new(region: TerrainRegion, options: CaptureOptions<'a>) -> Self {
        Self {
            region,
            is_empty: options.is_empty,
            max_segments: options.max_segments,
            columns: Vec::new(),
            material_ids: HashMap::new(),
            materials: Vec::new(),
            column: 0,
            y: region.height as i32 - 1,
            segment_count: 0,
            sampled: 0,
        }
    }

    /// Samples at most `budget` blocks; returns true once the region is complete.
    ///
    /// # Errors
    /// Fails when the source fails or the segment budget is exceeded.
    ///
    /// # Panics
    /// Panics only on internal inconsistency (segment checked before access).
    #[allow(clippy::missing_panics_doc)]
    pub fn step(&mut self, source: &dyn TerrainSource, budget: u64) -> Result<bool, CaptureError> {
        if source.min_y() != self.region.min_y || source.height() != self.region.height as i32 {
            return Err(CaptureError::WorldHeightChanged);
        }

        let mut remaining = budget;
        while remaining > 0 && self.column < self.region.area() {
            let local_x = self.column / self.region.depth;
            let local_z = self.column % self.region.depth;
            let x = self.region.origin_x + local_x as i32;
            let z = self.region.origin_z + local_z as i32;
            let sample = source
                .sample(x, self.region.min_y + self.y, z)
                .map_err(CaptureError::Source)?;
            self.sampled += 1;

            let mut span: i32 = 1;
            if self.y == self.region.height as i32 - 1
                && (self.is_empty)(&sample.material)
            {
                let top = source.top(x, z).map_err(CaptureError::Source)?;
                let first_air = (top - self.region.min_y + 1)
                    .clamp(0, self.region.height as i32 - 1);
                span = (self.y - first_air).max(1);
            }

            let material_id = if let Some(id) = self.material_ids.get(&sample.material) {
                *id
            } else {
                let id = self.materials.len() as u32;
                self.materials.push(sample.material.clone());
                self.material_ids.insert(sample.material.clone(), id);
                id
            };

            if self.columns.len() <= self.column {
                self.columns.resize_with(self.column + 1, Column::default);
            }
            let column = &mut self.columns[self.column];
            let extend = column
                .segments
                .last()
                .is_some_and(|previous: &Segment| {
                    previous.material_id == material_id
                        && previous.sky_light == sample.sky_light
                        && previous.block_light == sample.block_light
                });
            if extend {
                let segment = column.segments.last_mut().expect("checked above");
                segment.start_y = self.y - span + 1;
                segment.height += span;
            } else {
                self.segment_count += 1;
                if self.segment_count > self.max_segments {
                    return Err(CaptureError::TooComplex);
                }
                column.segments.push(Segment {
                    material_id,
                    start_y: self.y - span + 1,
                    height: span,
                    sky_light: sample.sky_light,
                    block_light: sample.block_light,
                });
            }

            self.y -= span;
            if self.y < 0 {
                self.y = self.region.height as i32 - 1;
                self.column += 1;
            }
            remaining -= 1;
        }
        Ok(self.column == self.region.area())
    }

    /// Blocks sampled by all steps so far.
    #[must_use]
    pub fn samples(&self) -> u64 {
        self.sampled
    }

    /// Fraction of the region covered, including collapsed spans.
    #[must_use]
    pub fn progress(&self) -> f64 {
        let height = self.region.height as i64;
        let covered =
            self.column as i64 * height + (self.region.height as i64 - 1 - i64::from(self.y));
        let total = self.region.area() as i64 * height;
        if total == 0 {
            return 0.0;
        }
        (covered as f64) / (total as f64)
    }

    /// Returns the captured section after sampling has finished.
    ///
    /// # Errors
    /// Fails when the capture is incomplete.
    pub fn result(self) -> Result<CapturedSection, CaptureError> {
        if self.column != self.region.area() {
            return Err(CaptureError::Invalid("capture is incomplete"));
        }
        Ok(CapturedSection {
            columns: self.columns,
            materials: self.materials,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Flat terrain: stone from `min_y` to `top(x, z)`, air above.
    struct FlatTerrain {
        min_y: i32,
        height: i32,
    }

    impl TerrainSource for FlatTerrain {
        fn height(&self) -> i32 {
            self.height
        }
        fn min_y(&self) -> i32 {
            self.min_y
        }
        fn sample(&self, x: i32, y: i32, z: i32) -> Result<TerrainSample, String> {
            let top = self.top(x, z)?;
            let material = if y <= top {
                "plains_DH-BSW_minecraft:stone"
            } else {
                "plains_DH-BSW_minecraft:air"
            };
            let lit = y > top;
            Ok(TerrainSample {
                material: material.to_string(),
                sky_light: u8::from(lit) * 15,
                block_light: 0,
            })
        }
        fn top(&self, x: i32, _z: i32) -> Result<i32, String> {
            Ok(self.min_y + 10 + (x.rem_euclid(2)))
        }
    }

    fn region() -> TerrainRegion {
        TerrainRegion {
            width: 4,
            depth: 4,
            height: 16,
            min_y: 0,
            origin_x: 0,
            origin_z: 0,
        }
    }

    #[test]
    fn captures_columns_and_collapses_air() {
        let mut capture = TerrainCapture::new(region(), CaptureOptions::dh());
        let terrain = FlatTerrain { min_y: 0, height: 16 };
        let mut done = false;
        while !done {
            done = capture.step(&terrain, 8).unwrap();
        }

        let result = capture.result().unwrap();
        // 16 columns, one empty column in each.
        assert_eq!(result.columns.len(), 16);
        // Materials: stone + air.
        assert!(result.materials.len() <= 2);

        // Column 0 (x=0): stone from y=0..=10 (11 blocks, one segment),
        // air collapsed from y=11..=15 (one segment via top()).
        let col = &result.columns[0].segments;
        assert_eq!(col.len(), 2, "stone segment + collapsed air segment");
        let stone_id = result
            .materials
            .iter()
            .position(|m| m.ends_with("minecraft:stone"))
            .expect("stone material present") as u32;
        // Top-down capture: the air segment comes first, stone last.
        let (air, stone) = (col.first().expect("air segment"), col.last().expect("stone segment"));
        assert_ne!(air.material_id, stone_id);
        assert_eq!(stone.material_id, stone_id);
        assert_eq!(stone.height, 11);
        assert_eq!(stone.start_y, 0);
        assert_eq!(air.start_y, 11);
        assert_eq!(air.height, 5);
    }

    #[test]
    fn budget_is_incremental() {
        let mut capture = TerrainCapture::new(region(), CaptureOptions::dh());
        let terrain = FlatTerrain { min_y: 0, height: 16 };
        assert!(!capture.step(&terrain, 1).unwrap());
        assert_eq!(capture.samples(), 1);
        assert!(capture.progress() > 0.0);
        assert!(!capture.step(&terrain, 5).unwrap());
        assert_eq!(capture.samples(), 6);
    }

    #[test]
    fn rejects_height_change() {
        let mut capture = TerrainCapture::new(region(), CaptureOptions::dh());
        let terrain = FlatTerrain { min_y: 0, height: 16 };
        capture.step(&terrain, 1).unwrap();
        let other = FlatTerrain { min_y: -64, height: 384 };
        assert_eq!(
            capture.step(&other, 1),
            Err(CaptureError::WorldHeightChanged)
        );
    }

    /// Checkerboard material by y parity: every sample is a new segment.
    struct Checker;

    impl TerrainSource for Checker {
        fn height(&self) -> i32 {
            8
        }
        fn min_y(&self) -> i32 {
            0
        }
        fn sample(&self, _x: i32, y: i32, _z: i32) -> Result<TerrainSample, String> {
            Ok(TerrainSample {
                material: format!("mat{}", y % 2),
                sky_light: 0,
                block_light: 0,
            })
        }
        fn top(&self, _x: i32, _z: i32) -> Result<i32, String> {
            Ok(0)
        }
    }

    #[test]
    fn segment_budget_is_enforced() {
        let region = TerrainRegion {
            width: 64,
            depth: 64,
            height: 8,
            min_y: 0,
            origin_x: 0,
            origin_z: 0,
        };
        let options = CaptureOptions {
            is_empty: &|_| false,
            max_segments: 4,
        };
        let mut capture = TerrainCapture::new(region, options);
        assert_eq!(capture.step(&Checker, 100), Err(CaptureError::TooComplex));
    }

    #[test]
    fn result_rejects_incomplete_capture() {
        let capture = TerrainCapture::new(region(), CaptureOptions::dh());
        assert_eq!(
            capture.result(),
            Err(CaptureError::Invalid("capture is incomplete"))
        );
    }
}
