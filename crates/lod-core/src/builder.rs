//! DH LOD section (FullDataSourceV2DTO) builder.
//!
//! Encodes captured terrain columns into the DH v1 DTO layout. Adapted from DH
//! Support FullBuilder and Lod. SPDX-License-Identifier: GPL-3.0-or-later.
#![allow(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::doc_markdown
)]

use crate::bytes::{DecodeError, Writer};
use crate::message::Section;
use crate::{SECTION_AREA, SECTION_DETAIL, SECTION_SIZE_BLOCKS};

/// Maximum material segments retained for one LOD section.
pub const MAX_POINTS_PER_SECTION: usize = 131_072;

/// `DTO.dataChecksum`, left unset.
const DATA_CHECKSUM: i32 = 0;
/// `EDhApiWorldGenerationStep.LIGHT`: every column is fully generated.
const GENERATION_STEP_LIGHT: u8 = 0o11;
/// `EDhApiWorldCompressionMode.MERGE_SAME_BLOCKS` for every column.
const WORLD_COMPRESSION_MERGE_SAME_BLOCKS: u8 = 0;
/// `DTO.DATA_FORMAT.V1_NO_ADJACENT_DATA`, so the four adjacent-data arrays stay empty.
const DATA_FORMAT_V1_NO_ADJACENT_DATA: u8 = 1;
/// `EDhApiDataCompressionMode.UNCOMPRESSED`.
const DATA_COMPRESSION_UNCOMPRESSED: u8 = 0;

/// Size of a DTO whose arrays are all empty: position, checksum, seven array
/// lengths, format and compression bytes, three flags and two timestamps.
pub const MIN_DTO_BYTES: usize = 8 + 4 + 7 * 4 + 2 + 3 + 2 * 8;

/// One vertical material segment in a captured terrain column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    /// Height of the segment top (block Y coordinate).
    pub height: i32,
    /// Y coordinate where the segment starts.
    pub start_y: i32,
    /// Sky light (0-15).
    pub sky_light: u8,
    /// Block light (0-15).
    pub block_light: u8,
    /// DH material id referencing the section's mapping.
    pub material_id: u32,
}

/// A captured terrain column: an ordered list of segments.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Column {
    /// Vertical segments, bottom to top.
    pub segments: Vec<Segment>,
}

/// Terrain capture result for one LOD section, ready to encode.
#[derive(Debug, Clone, PartialEq, Eq)]
#[derive(Default)]
pub struct CapturedSection {
    /// Columns in DH order (z * width + x).
    pub columns: Vec<Column>,
    /// Material names referenced by segments, index = material id.
    pub materials: Vec<String>,
}

impl CapturedSection {
    /// Builds a captured section from pre-sampled columns and a material mapping.
    ///
    /// # Errors
    /// Fails when segment lights are out of range.
    pub fn validate(&self) -> Result<(), DecodeError> {
        for column in &self.columns {
            for segment in &column.segments {
                if segment.sky_light > 15 || segment.block_light > 15 {
                    return Err(DecodeError::InvalidValue("terrain light"));
                }
            }
        }
        Ok(())
    }
}

/// Encodes a captured section in the supported DH v1 DTO layout.
///
/// # Errors
/// Fails when segment lights are out of range or the section is inconsistent.
pub fn encode_section(
    section: &Section,
    captured: &CapturedSection,
    now: u64,
) -> Result<Vec<u8>, DecodeError> {
    if section.detail != SECTION_DETAIL {
        return Err(DecodeError::InvalidValue("section detail"));
    }
    captured.validate()?;

    let mut column_data = Writer::new();
    for column in &captured.columns {
        column_data.short(column.segments.len() as u16);
        for segment in &column.segments {
            let high = (segment.height as u32)
                | (segment.start_y as u32) << 12
                | u32::from(segment.sky_light) << 24
                | u32::from(segment.block_light) << 28;
            column_data.words(high, segment.material_id);
        }
    }

    let mut mapping_data = Writer::new();
    mapping_data.int(captured.materials.len() as i32);
    for material in &captured.materials {
        mapping_data.string(material);
    }

    let mut w = Writer::new();
    w.words(section.high, section.low)
        .int(DATA_CHECKSUM)
        .blob(&column_data.finish())
        .blob(&[]) // north adjacent data
        .blob(&[]) // south adjacent data
        .blob(&[]) // east adjacent data
        .blob(&[]) // west adjacent data
        .blob(&vec![GENERATION_STEP_LIGHT; SECTION_AREA])
        .blob(&vec![WORLD_COMPRESSION_MERGE_SAME_BLOCKS; SECTION_AREA])
        .blob(&mapping_data.finish())
        .byte(DATA_FORMAT_V1_NO_ADJACENT_DATA)
        .byte(DATA_COMPRESSION_UNCOMPRESSED)
        .bool(true) // applyToParent
        .bool(false) // applyToChildren
        .bool(false) // regenerate
        .timestamp(now)? // lastModifiedUnixDateTime
        .timestamp(now)?; // createdUnixDateTime
    Ok(w.finish())
}

/// Computes the world-region (block coordinates) covered by an LOD section.
#[must_use]
pub fn section_region(section: &Section, min_y: i32, height: i32) -> (i32, i32, i32, i32, i32, i32) {
    let size = SECTION_SIZE_BLOCKS as i32;
    let x = section.x * size;
    let z = section.z * size;
    (x, min_y, z, x + size, min_y + height, z + size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytes::Reader;
    use crate::message::Section;

    #[test]
    fn encodes_minimal_section() {
        let section = Section::encode(0, 0);
        let material = "_DH-BSW_minecraft:air".to_string();
        let captured = CapturedSection {
            columns: vec![Column {
                segments: vec![Segment {
                    height: 64,
                    start_y: -64,
                    sky_light: 15,
                    block_light: 0,
                    material_id: 0,
                }],
            }],
            materials: vec![material],
        };
        let bytes = encode_section(&section, &captured, 1_700_000_000_000).unwrap();
        assert!(bytes.len() >= MIN_DTO_BYTES);
        let mut pw = Writer::new();
        pw.words(section.high, section.low);
        let packed = pw.finish();
        assert_eq!(&bytes[..8], &packed[..]);
    }

    #[test]
    fn rejects_bad_detail_and_lights() {
        let mut section = Section::encode(0, 0);
        section.detail = 4;
        let captured = CapturedSection::default();
        assert_eq!(
            encode_section(&section, &captured, 0),
            Err(DecodeError::InvalidValue("section detail"))
        );

        let section = Section::encode(0, 0);
        let captured = CapturedSection {
            columns: vec![Column {
                segments: vec![Segment {
                    height: 1,
                    start_y: 0,
                    sky_light: 16,
                    block_light: 0,
                    material_id: 0,
                }],
            }],
            materials: vec![],
        };
        assert_eq!(
            encode_section(&section, &captured, 0),
            Err(DecodeError::InvalidValue("terrain light"))
        );
    }

    #[test]
    fn section_region_covers_section_blocks() {
        let section = Section::encode(2, -3);
        let (x0, y0, z0, x1, y1, z1) = section_region(&section, -64, 384);
        assert_eq!(x0, 2 * 64);
        assert_eq!(x1, x0 + 64);
        assert_eq!(z0, -3 * 64);
        assert_eq!(z1, z0 + 64);
        assert_eq!((y0, y1), (-64, 320));
    }

    #[test]
    fn dto_layout_is_stable() {
        let section = Section::encode(1, 1);
        let captured = CapturedSection {
            columns: vec![
                Column::default(),
                Column {
                    segments: vec![Segment {
                        height: 10,
                        start_y: 0,
                        sky_light: 3,
                        block_light: 4,
                        material_id: 1,
                    }],
                },
            ],
            materials: vec!["a".into(), "b".into()],
        };
        let bytes = encode_section(&section, &captured, 7).unwrap();

        let mut r = Reader::new(&bytes);
        r.words().unwrap();
        r.int().unwrap();
        let columns = r.blob().unwrap();
        assert_eq!(columns, {
            let mut w = Writer::new();
            w.short(0);
            w.short(1);
            w.words((0b1010 | (0b0011 << 24) | (0b0100 << 28)) as u32, 1);
            w.finish()
        });
        for _ in 0..4 {
            assert_eq!(r.blob().unwrap(), &[] as &[u8]);
        }
        assert_eq!(r.blob().unwrap().len(), SECTION_AREA);
        assert_eq!(r.blob().unwrap().len(), SECTION_AREA);
        let mapping = r.blob().unwrap();
        let mut mw = Writer::new();
        mw.int(2).string("a").string("b");
        let mapping_expected = mw.finish();
        assert_eq!(mapping, &mapping_expected[..]);
        assert_eq!(r.byte().unwrap(), 1);
        assert_eq!(r.byte().unwrap(), 0);
        assert!(r.bool().unwrap());
        assert!(!r.bool().unwrap());
        assert!(!r.bool().unwrap());
        assert_eq!(r.timestamp().unwrap(), 7);
        assert_eq!(r.timestamp().unwrap(), 7);
        r.end().unwrap();
    }
}
