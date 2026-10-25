//! Distant Horizons LOD protocol (network protocol 16) core.
//!
//! Byte-level codec, client message decoding, server message encoding and the
//! LOD section (DTO) builder used by the `ArchiDistantVoxy` Pumpkin plugin.
//!
//! Adapted from DH Support (Copyright (C) 2024 Jim C K Flaten) and Distant
//! Horizons core (Copyright (C) 2020 James Seibel). SPDX-License-Identifier:
//! GPL-3.0-or-later.

pub mod bytes;
pub mod cache;
pub mod builder;
pub mod message;
pub mod throttle;
pub mod transfer;

/// Distant Horizons client release this implementation targets.
pub const DH_VERSION: &str = "3.3.4";
/// Network protocol used by the supported Distant Horizons release.
pub const PROTOCOL: u16 = 16;
/// Block-detail value for an LOD section.
pub const SECTION_DETAIL: u8 = 6;
/// Number of blocks along one side of an LOD section.
pub const SECTION_SIZE_BLOCKS: u32 = 1 << SECTION_DETAIL;
/// Number of block columns in one LOD section.
pub const SECTION_AREA: usize = (SECTION_SIZE_BLOCKS * SECTION_SIZE_BLOCKS) as usize;
/// Maximum payload size for one DH transfer packet.
pub const TRANSFER_PACKET_BYTES: usize = 30_000;
/// Plugin channel shared with the released DH client.
pub const CHANNEL: &str = "distant_horizons:msg";
