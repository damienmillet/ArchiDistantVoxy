//! LSS (LOD Server Support) wire protocol v20 for Voxy clients.
//!
//! Server-side port of the LSS networking layer (MIT, © 2026 VoX): plugin
//! channels, handshake, batch requests, and the v20 voxel-column section-array
//! encoder shared with the DH path. SPDX-License-Identifier: GPL-3.0-or-later.

pub mod column;
pub mod message;

/// LSS protocol version implemented here.
pub const PROTOCOL_VERSION: i32 = 20;
/// v16 legacy compat rung.
pub const V16_COMPAT_PROTOCOL_VERSION: i32 = 16;

/// Plugin channels, one per payload type (legacy servers discard unknown
/// channels, hence the sidecar doctrine).
pub const CHANNEL_HANDSHAKE: &str = "lss:handshake_c2s";
pub const CHANNEL_CHUNK_REQUEST: &str = "lss:batch_chunk_req";
pub const CHANNEL_SESSION_CONFIG: &str = "lss:session_config";
pub const CHANNEL_DIRTY_COLUMNS: &str = "lss:dirty_columns";
pub const CHANNEL_VOXEL_COLUMN: &str = "lss:voxel_column";
pub const CHANNEL_BATCH_RESPONSE: &str = "lss:batch_response";
pub const CHANNEL_CLIENT_INFO: &str = "lss:client_info";

/// Capabilities announced in the handshake.
pub const CAPABILITY_VOXEL_COLUMNS: i32 = 1;
/// Capabilities announced in the handshake.
pub const CAPABILITY_ZSTD_COLUMNS: i32 = 2;
/// Capabilities announced in the handshake.
pub const CAPABILITY_FAR_PLAYERS: i32 = 4;
/// Capabilities announced in the handshake.
pub const CAPABILITY_MAP_POLICY: i32 = 8;

/// Maximum batch chunk requests per payload.
pub const MAX_BATCH_CHUNK_REQUESTS: i32 = 1024;
/// Maximum batch responses per payload.
pub const MAX_BATCH_RESPONSES: i32 = 4096;
/// Maximum section-array byte size.
pub const MAX_SECTIONS_SIZE: usize = 2_097_152;
/// Maximum dimension string length in columns.
pub const MAX_DIMENSION_STRING_LENGTH: usize = 256;

/// Column provenance tags (`COLUMN_SOURCE_*`).
pub const COLUMN_SOURCE_IN_MEMORY: i8 = 0;
/// Column provenance tags (`COLUMN_SOURCE_*`).
pub const COLUMN_SOURCE_DISK: i8 = 1;
/// Column provenance tags (`COLUMN_SOURCE_*`).
pub const COLUMN_SOURCE_GENERATION: i8 = 2;
/// Column provenance tags (`COLUMN_SOURCE_*`).
pub const COLUMN_SOURCE_STORE: i8 = 3;

/// Section bytes ship raw.
pub const COLUMN_CODEC_RAW: i8 = 0;
/// Section bytes ship as a zstd frame (not produced here).
pub const COLUMN_CODEC_ZSTD: i8 = 1;

/// Batch response type: up-to-date (client copy current).
pub const RESPONSE_UP_TO_DATE: i8 = 1;
/// Batch response type: not generated at the requested position.
pub const RESPONSE_NOT_GENERATED: i8 = 2;

/// Packs a chunk position the way LSS does: `(cx as i64) << 32 | (cz as u32)`.
#[must_use]
pub fn pack_position(cx: i32, cz: i32) -> i64 {
    (i64::from(cx)) << 32 | i64::from(cz) & 0xFFFF_FFFF
}

/// Unpacks the X chunk coordinate from a packed position.
#[must_use]
pub fn unpack_x(packed: i64) -> i32 {
    (packed >> 32) as i32
}

/// Unpacks the Z chunk coordinate from a packed position (low 32 bits).
#[must_use]
#[allow(clippy::cast_possible_truncation)]
pub fn unpack_z(packed: i64) -> i32 {
    packed as i32
}

/// Chebyshev distance between two chunk positions.
#[must_use]
pub fn chebyshev_distance(ax: i32, az: i32, bx: i32, bz: i32) -> i32 {
    (ax - bx).abs().max((az - bz).abs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_roundtrip() {
        for &(x, z) in &[(0, 0), (1, -1), (-100, 1000), (2_000_000_000, -2_000_000_000)] {
            let packed = pack_position(x, z);
            assert_eq!(unpack_x(packed), x);
            assert_eq!(unpack_z(packed), z);
        }
    }

    #[test]
    fn chebyshev_is_square_distance() {
        assert_eq!(chebyshev_distance(0, 0, 3, 2), 3);
        assert_eq!(chebyshev_distance(3, 2, 0, 0), 3);
        assert_eq!(chebyshev_distance(-5, -5, -8, -1), 4);
    }
}
