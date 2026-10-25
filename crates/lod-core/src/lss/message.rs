//! LSS payload encoders and decoders (v20, with the v16 legacy echo).
//!
//! Wire shapes ported from the LSS `StreamCodecs`: netty big-endian fixed-width
//! integers, `VarInts`, VarInt-prefixed UTF. SPDX-License-Identifier:
//! GPL-3.0-or-later.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

use crate::bytes::{DecodeError, Reader, Writer};
use crate::lss::{
    CHANNEL_CHUNK_REQUEST, CHANNEL_CLIENT_INFO, CHANNEL_DIRTY_COLUMNS, CHANNEL_HANDSHAKE,
    MAX_BATCH_CHUNK_REQUESTS, MAX_DIMENSION_STRING_LENGTH, MAX_SECTIONS_SIZE, PROTOCOL_VERSION,
    RESPONSE_NOT_GENERATED, RESPONSE_UP_TO_DATE, V16_COMPAT_PROTOCOL_VERSION,
};

/// Maximum payload size accepted for a single LSS client message.
pub const MAX_LSS_MESSAGE_BYTES: usize = MAX_SECTIONS_SIZE + 1024;

/// Client handshake: announced protocol version and capability bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Handshake {
    /// Protocol version the client speaks.
    pub protocol_version: i32,
    /// Capability bit set.
    pub capabilities: i32,
}

/// Decodes a client handshake (`lss:handshake_c2s`).
///
/// # Errors
/// Fails when the payload is truncated.
pub fn decode_handshake(data: &[u8]) -> Result<Handshake, DecodeError> {
    let mut input = Reader::new(data);
    let protocol_version = input.varint()?;
    let capabilities = input.varint()?;
    input.end()?;
    Ok(Handshake {
        protocol_version,
        capabilities,
    })
}

/// A single entry of a client batch chunk request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkRequest {
    /// Packed chunk position.
    pub packed_position: i64,
    /// The client's cached column timestamp (0 = unknown).
    pub client_timestamp: i64,
}

/// Decodes a client batch chunk request (`lss:batch_chunk_req`).
///
/// # Errors
/// Fails when the payload is truncated or the count is out of range.
pub fn decode_batch_chunk_request(data: &[u8]) -> Result<Vec<ChunkRequest>, DecodeError> {
    let mut input = Reader::new(data);
    let count = input.varint()?;
    if !(0..=MAX_BATCH_CHUNK_REQUESTS).contains(&count) {
        return Err(DecodeError::InvalidValue("batch request count"));
    }
    let mut requests = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let packed_position = input.i64()?;
        let client_timestamp = input.i64()?;
        requests.push(ChunkRequest {
            packed_position,
            client_timestamp,
        });
    }
    input.end()?;
    Ok(requests)
}

/// Session configuration pushed to the client (`lss:session_config`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionConfig {
    /// Protocol version echoed to the client.
    pub protocol_version: i32,
    /// Whether LOD service is enabled for this session.
    pub enabled: bool,
    /// LOD distance in chunks.
    pub lod_distance_chunks: i32,
    /// Whether on-demand generation is served.
    pub generation_enabled: bool,
}

/// Encodes the session config reply for a v20 handshake.
///
/// # Panics
/// Never panics: all fields are caller-validated.
#[must_use]
pub fn encode_session_config(config: SessionConfig) -> Vec<u8> {
    let mut out = Writer::new();
    out.varint(config.protocol_version);
    out.bool(config.enabled);
    out.varint(config.lod_distance_chunks);
    out.bool(config.generation_enabled);
    // v20-only append: server data version; the v16 echo omits it.
    if config.protocol_version == PROTOCOL_VERSION {
        out.varint(0);
    }
    out.finish()
}

/// Encodes the v16 legacy session config reply (6-field layout).
///
/// # Panics
/// Never panics.
#[must_use]
pub fn encode_session_config_v16(
    enabled: bool,
    lod_distance_chunks: i32,
    sync_cap: i32,
    gen_cap: i32,
    generation_enabled: bool,
) -> Vec<u8> {
    let mut out = Writer::new();
    out.varint(V16_COMPAT_PROTOCOL_VERSION);
    out.bool(enabled);
    out.varint(lod_distance_chunks);
    out.varint(sync_cap);
    out.varint(gen_cap);
    out.bool(generation_enabled);
    out.finish()
}

/// One entry of a batch response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchResponseEntry {
    /// Response type tag (`RESPONSE_*`).
    pub response_type: i8,
    /// Packed chunk position the entry answers.
    pub packed_position: i64,
}

/// Encodes a batch response (`lss:batch_response`).
///
/// # Panics
/// Never panics: count is bounded by the slice length.
#[must_use]
pub fn encode_batch_response(entries: &[BatchResponseEntry]) -> Vec<u8> {
    let mut out = Writer::new();
    out.varint(entries.len() as i32);
    for entry in entries {
        out.byte(entry.response_type as u8);
        out.i64(entry.packed_position);
    }
    out.finish()
}

/// Builds an up-to-date batch response for the given packed positions.
///
/// # Panics
/// Never panics.
#[must_use]
pub fn up_to_date(positions: &[i64]) -> Vec<u8> {
    encode_batch_response(
        &positions
            .iter()
            .map(|&p| BatchResponseEntry {
                response_type: RESPONSE_UP_TO_DATE,
                packed_position: p,
            })
            .collect::<Vec<_>>(),
    )
}

/// Builds a not-generated batch response for the given packed positions.
///
/// # Panics
/// Never panaps.
#[must_use]
pub fn not_generated(positions: &[i64]) -> Vec<u8> {
    encode_batch_response(
        &positions
            .iter()
            .map(|&p| BatchResponseEntry {
                response_type: RESPONSE_NOT_GENERATED,
                packed_position: p,
            })
            .collect::<Vec<_>>(),
    )
}

/// Encodes a voxel column (`lss:voxel_column`, protocol 19+ layout with the
/// source and codec bytes).
///
/// # Panics
/// Never panics: `section_bytes` is copied as given.
#[must_use]
pub fn encode_voxel_column(
    chunk_x: i32,
    chunk_z: i32,
    dimension: &str,
    column_timestamp: i64,
    source: i8,
    codec: i8,
    section_bytes: &[u8],
) -> Vec<u8> {
    let mut out = Writer::new();
    out.int(chunk_x);
    out.int(chunk_z);
    out.string_bounded(dimension, MAX_DIMENSION_STRING_LENGTH);
    out.i64(column_timestamp);
    out.byte(source as u8);
    out.byte(codec as u8);
    out.blob(section_bytes);
    out.finish()
}

/// The channel a decoded client payload belongs to, when recognized.
#[must_use]
pub fn client_channel(channel: &str) -> Option<&'static str> {
    let channels = [
        (CHANNEL_HANDSHAKE, CHANNEL_HANDSHAKE),
        (CHANNEL_CHUNK_REQUEST, CHANNEL_CHUNK_REQUEST),
        (CHANNEL_CLIENT_INFO, CHANNEL_CLIENT_INFO),
        (CHANNEL_DIRTY_COLUMNS, CHANNEL_DIRTY_COLUMNS),
    ];
    channels
        .iter()
        .find(|(name, _)| *name == channel)
        .map(|(_, canonical)| *canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lss::{
        pack_position, unpack_x, unpack_z, CHANNEL_CHUNK_REQUEST,
    };

    #[test]
    fn handshake_roundtrip() {
        let mut w = Writer::new();
        w.varint(20).varint(3);
        let decoded = decode_handshake(&w.finish()).unwrap();
        assert_eq!(decoded.protocol_version, 20);
        assert_eq!(decoded.capabilities, 3);
    }

    #[test]
    fn handshake_rejects_trailing_bytes() {
        let mut w = Writer::new();
        w.varint(20).varint(0).byte(9);
        assert!(decode_handshake(&w.finish()).is_err());
    }

    #[test]
    fn batch_request_roundtrip_and_bounds() {
        let mut w = Writer::new();
        w.varint(2).i64(pack_position(3, 4)).i64(100).i64(pack_position(-5, 6)).i64(0);
        let requests = decode_batch_chunk_request(&w.finish()).unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(unpack_x(requests[0].packed_position), 3);
        assert_eq!(unpack_z(requests[0].packed_position), 4);
        assert_eq!(requests[0].client_timestamp, 100);

        let mut bad = Writer::new();
        bad.varint(MAX_BATCH_CHUNK_REQUESTS + 1);
        assert!(decode_batch_chunk_request(&bad.finish()).is_err());
    }

    #[test]
    fn session_config_v20_has_data_version_append() {
        let bytes = encode_session_config(SessionConfig {
            protocol_version: PROTOCOL_VERSION,
            enabled: true,
            lod_distance_chunks: 512,
            generation_enabled: true,
        });
        let mut r = Reader::new(&bytes);
        assert_eq!(r.varint().unwrap(), 20);
        assert!(r.bool().unwrap());
        assert_eq!(r.varint().unwrap(), 512);
        assert!(r.bool().unwrap());
        assert_eq!(r.varint().unwrap(), 0);
        r.end().unwrap();
    }

    #[test]
    fn session_config_v16_is_six_fields() {
        let bytes = encode_session_config_v16(true, 256, 8, 4, true);
        let mut r = Reader::new(&bytes);
        assert_eq!(r.varint().unwrap(), 16);
        assert!(r.bool().unwrap());
        assert_eq!(r.varint().unwrap(), 256);
        assert_eq!(r.varint().unwrap(), 8);
        assert_eq!(r.varint().unwrap(), 4);
        assert!(r.bool().unwrap());
        r.end().unwrap();
    }

    #[test]
    fn batch_response_roundtrip() {
        let entries = [
            BatchResponseEntry {
                response_type: RESPONSE_UP_TO_DATE,
                packed_position: pack_position(1, 2),
            },
            BatchResponseEntry {
                response_type: RESPONSE_NOT_GENERATED,
                packed_position: pack_position(3, 4),
            },
        ];
        let bytes = encode_batch_response(&entries);
        let mut r = Reader::new(&bytes);
        assert_eq!(r.varint().unwrap(), 2);
        assert_eq!(r.byte().unwrap() as i8, RESPONSE_UP_TO_DATE);
        assert_eq!(unpack_x(r.i64().unwrap()), 1);
        assert_eq!(r.byte().unwrap() as i8, RESPONSE_NOT_GENERATED);
        assert_eq!(unpack_z(r.i64().unwrap()), 4);
        r.end().unwrap();
    }

    #[test]
    fn voxel_column_layout() {
        let bytes = encode_voxel_column(7, -9, "minecraft:overworld", 123_456, 2, 0, &[1, 2, 3]);
        let mut r = Reader::new(&bytes);
        assert_eq!(r.int().unwrap(), 7);
        assert_eq!(r.int().unwrap(), -9);
        assert_eq!(r.string().unwrap(), "minecraft:overworld");
        assert_eq!(r.i64().unwrap(), 123_456);
        assert_eq!(r.byte().unwrap() as i8, 2);
        assert_eq!(r.byte().unwrap() as i8, 0);
        assert_eq!(r.blob().unwrap(), &[1, 2, 3]);
        r.end().unwrap();
    }

    #[test]
    fn client_channel_routes_known_channels() {
        assert_eq!(client_channel("lss:handshake_c2s"), Some(CHANNEL_HANDSHAKE));
        assert_eq!(client_channel("lss:batch_chunk_req"), Some(CHANNEL_CHUNK_REQUEST));
        assert_eq!(client_channel("unknown:channel"), None);
        assert_eq!(client_channel("lss:voxel_column"), None);
    }
}
