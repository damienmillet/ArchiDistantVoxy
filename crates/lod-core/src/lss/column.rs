//! v20 voxel-column section-array encoder (the LSS wire body).
//!
//! Layout (ported from LSS `WireSectionCursor` emit, `Layout.V20`):
//! `VarInt dictCount`, `dictCount × writeUtf(identity)`, `VarInt sectionCount`,
//! then per section: `byte sectionY`, `short nonEmptyBlockCount`,
//! `short fluidCount`, block container, biome container, block light, sky
//! light. Containers: `byte bits`, `VarInt paletteSize`, palette entries as
//! `VarInts`, then the packed longs. Light: `byte 0/1`, 2048 bytes when present.
//!
//! SPDX-License-Identifier: GPL-3.0-or-later.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

use crate::bytes::Writer;

/// Entries in a block container (16³ = 4096).
pub const BLOCK_ENTRIES: usize = 4096;
/// Entries in a biome container (4³×4 = 64).
pub const BIOME_ENTRIES: usize = 64;
/// Light layer size in bytes (16³ nibbles).
pub const LIGHT_BYTES: usize = 2048;

/// v20 block palette bit width: `max(4, min(12, ceillog2(n)))`, 0 when n <= 1.
#[must_use]
pub fn v20_block_bits(palette_size: usize) -> u8 {
    v20_bits(palette_size, true)
}

/// v20 biome palette bit width: `ceillog2(n)`, 0 when n <= 1.
#[must_use]
pub fn v20_biome_bits(palette_size: usize) -> u8 {
    v20_bits(palette_size, false)
}

fn v20_bits(palette_size: usize, is_blocks: bool) -> u8 {
    if palette_size <= 1 {
        return 0;
    }
    let ceillog2 = (palette_size - 1).ilog2() + 1;
    if is_blocks {
        u8::try_from(ceillog2.clamp(4, 12)).unwrap_or(12)
    } else {
        u8::try_from(ceillog2.clamp(0, 6)).unwrap_or(6)
    }
}

/// One section of a voxel column: palettes of dictionary indices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// Section Y index (world Y / 16).
    pub section_y: i8,
    /// Non-empty block count (vanilla wire header).
    pub non_empty_block_count: i16,
    /// Fluid count (vanilla wire header).
    pub fluid_count: i16,
    /// Block palette: dictionary indices.
    pub block_palette: Vec<usize>,
    /// Block data: packed indices at the block width.
    pub block_data: Vec<i64>,
    /// Biome palette: dictionary indices.
    pub biome_palette: Vec<usize>,
    /// Biome data: packed indices at the biome width.
    pub biome_data: Vec<i64>,
    /// Block light layer, when present.
    pub block_light: Option<[u8; LIGHT_BYTES]>,
    /// Sky light layer, when present.
    pub sky_light: Option<[u8; LIGHT_BYTES]>,
}

/// Packs `values` (palette indices) at `bits` per value, LSB-first per long,
/// vanilla-style: `values_per_long = 64 / bits`, no straddle.
///
/// # Panics
/// Panics when `bits` is 0 or > 32.
#[must_use]
pub fn pack_indices(values: &[usize], bits: u8) -> Vec<i64> {
    assert!(bits > 0 && bits <= 32, "bits out of range");
    let per_long = (64 / bits) as usize;
    let mut longs = Vec::with_capacity(values.len().div_ceil(per_long));
    let mut current: u64 = 0;
    let mut in_long: usize = 0;
    let mask: u64 = (1u64 << bits) - 1;
    for &value in values {
        current |= (value as u64 & mask) << (in_long * usize::from(bits));
        in_long += 1;
        if in_long == per_long {
            longs.push(current as i64);
            current = 0;
            in_long = 0;
        }
    }
    if in_long > 0 {
        longs.push(current as i64);
    }
    longs
}

/// Encodes a v20 voxel-column body.
///
/// # Panics
/// Panics when a dictionary index is out of range or a section is malformed.
#[must_use]
pub fn encode_column(dictionary: &[String], sections: &[Section]) -> Vec<u8> {
    let mut out = Writer::new();
    out.varint(dictionary.len() as i32);
    for identity in dictionary {
        out.string_bounded(identity, crate::lss::MAX_DIMENSION_STRING_LENGTH);
    }
    out.varint(sections.len() as i32);
    for section in sections {
        out.byte(section.section_y as u8);
        out.short(section.non_empty_block_count as u16);
        out.short(section.fluid_count as u16);
        write_container(
            &mut out,
            &section.block_palette,
            &section.block_data,
            BLOCK_ENTRIES,
        );
        write_container(
            &mut out,
            &section.biome_palette,
            &section.biome_data,
            BIOME_ENTRIES,
        );
        write_light(&mut out, section.block_light.as_ref());
        write_light(&mut out, section.sky_light.as_ref());
    }
    out.finish()
}

/// Encodes an empty column (0 sections, 0 dictionary entries — the clear frame).
#[must_use]
pub fn encode_clear_column() -> Vec<u8> {
    let mut out = Writer::new();
    out.varint(0).varint(0);
    out.finish()
}

fn write_container(out: &mut Writer, palette: &[usize], data: &[i64], entries: usize) {
    let bits = v20_bits(palette.len(), entries == BLOCK_ENTRIES);
    out.byte(bits);
    if bits == 0 {
        let single = palette.first().copied().unwrap_or(0);
        out.varint(single as i32);
        return;
    }
    out.varint(palette.len() as i32);
    for value in palette {
        out.varint(*value as i32);
    }
    let per_long = usize::from(64 / bits);
    let expected = entries.div_ceil(per_long);
    assert_eq!(data.len(), expected, "container data length mismatch");
    for long in data {
        out.i64(*long);
    }
}

fn write_light(out: &mut Writer, light: Option<&[u8; LIGHT_BYTES]>) {
    match light {
        None => {
            out.byte(0);
        }
        Some(bytes) => {
            out.byte(1);
            for b in bytes {
                out.byte(*b);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytes::Reader;

    fn single_section() -> Section {
        Section {
            section_y: 4,
            non_empty_block_count: 4096,
            fluid_count: 0,
            block_palette: vec![0, 1, 2],
            block_data: pack_indices(&vec![0; BLOCK_ENTRIES], 4),
            biome_palette: vec![0],
            biome_data: Vec::new(),
            block_light: None,
            sky_light: Some([15; LIGHT_BYTES]),
        }
    }

    #[test]
    fn bits_follow_v20_rules() {
        assert_eq!(v20_block_bits(1), 0);
        assert_eq!(v20_block_bits(2), 4);
        assert_eq!(v20_block_bits(16), 4);
        assert_eq!(v20_block_bits(17), 5);
        assert_eq!(v20_block_bits(4096), 12);
        assert_eq!(v20_biome_bits(1), 0);
        assert_eq!(v20_biome_bits(2), 1);
        assert_eq!(v20_biome_bits(64), 6);
    }

    #[test]
    fn packing_roundtrips_lsfirst() {
        let values: Vec<usize> = (0..100).map(|i| i % 7).collect();
        let longs = pack_indices(&values, 4);
        let per_long = 16;
        assert_eq!(longs.len(), values.len().div_ceil(per_long));
        for (i, &v) in values.iter().enumerate() {
            let long = longs[i / per_long] as u64;
            let shift = (i % per_long) * 4;
            assert_eq!(((long >> shift) & 0xF) as usize, v);
        }
    }

    #[test]
    fn column_layout_is_v20() {
        let dictionary = vec!["minecraft:stone".to_string(), "minecraft:dirt".to_string()];
        let bytes = encode_column(&dictionary, &[single_section()]);
        let mut r = Reader::new(&bytes);
        assert_eq!(r.varint().unwrap(), 2);
        assert_eq!(r.string().unwrap(), "minecraft:stone");
        assert_eq!(r.string().unwrap(), "minecraft:dirt");
        assert_eq!(r.varint().unwrap(), 1);
        assert_eq!(r.byte().unwrap(), 4);
        // nonEmpty + fluid shorts
        assert_eq!(r.short().unwrap(), 4096);
        assert_eq!(r.short().unwrap(), 0);
        // block container: bits 4, palette 3
        assert_eq!(r.byte().unwrap(), 4);
        assert_eq!(r.varint().unwrap(), 3);
        assert_eq!(r.varint().unwrap(), 0);
        assert_eq!(r.varint().unwrap(), 1);
        assert_eq!(r.varint().unwrap(), 2);
        for _ in 0..256 {
            r.i64().unwrap();
        }
        // biome container: bits 0, single entry
        assert_eq!(r.byte().unwrap(), 0);
        assert_eq!(r.varint().unwrap(), 0);
        // block light absent
        assert_eq!(r.byte().unwrap(), 0);
        // sky light present: flag + 2048 bytes
        assert_eq!(r.byte().unwrap(), 1);
        for _ in 0..LIGHT_BYTES {
            assert_eq!(r.byte().unwrap(), 15);
        }
        r.end().unwrap();
    }

    #[test]
    fn clear_column_is_two_zero_varints() {
        let bytes = encode_clear_column();
        assert_eq!(bytes, vec![0, 0]);
    }
}
