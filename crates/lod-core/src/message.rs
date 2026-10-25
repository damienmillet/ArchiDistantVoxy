//! DH client-to-server message decoding and server-to-client message encoding.
//!
//! Adapted from DH Support. SPDX-License-Identifier: GPL-3.0-or-later.
#![allow(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::similar_names
)]

use crate::bytes::{DecodeError, Reader, Writer};
use crate::{PROTOCOL, SECTION_DETAIL, TRANSFER_PACKET_BYTES};

/// Coordinates of a DH block-detail section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Section {
    /// Detail level (blocks per side = 2^detail).
    pub detail: u8,
    /// High 32 bits of the packed section position.
    pub high: u32,
    /// Low 32 bits of the packed section position.
    pub low: u32,
    /// Section X coordinate.
    pub x: i32,
    /// Section Z coordinate.
    pub z: i32,
}

impl Section {
    /// Packs block-detail section coordinates the way [`read_section`] unpacks them.
    #[must_use]
    pub fn encode(x: i32, z: i32) -> Self {
        let packed_x = x as u32 & 0x0fff_ffff;
        let packed_z = z as u32 & 0x0fff_ffff;
        Self {
            detail: SECTION_DETAIL,
            high: (packed_z << 4) | (packed_x >> 24),
            low: (packed_x << 8) | u32::from(SECTION_DETAIL),
            x,
            z,
        }
    }
}

/// Reads a packed section position without losing any of its 64 bits.
///
/// # Errors
/// Fails when the payload is truncated.
pub fn read_section(input: &mut Reader<'_>) -> Result<Section, DecodeError> {
    let (high, low) = input.words()?;
    let x = ((((low >> 8) | ((high & 15) << 24)) << 4) as i32) >> 4;
    let z = (((high >> 4) << 4) as i32) >> 4;
    Ok(Section {
        high,
        low,
        detail: (low & 0xff) as u8,
        x,
        z,
    })
}

/// Settings exchanged in a DH session configuration message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionConfiguration {
    /// Client-requested bandwidth cap in KB/s.
    pub bandwidth_kbps: i32,
    /// Distance (chunks) the client wants distant generation for.
    pub generation_distance: i32,
    /// Client generator plan (0-3).
    pub generation_plan: u8,
    /// Client-requested generation rate.
    pub generation_rate: i32,
    /// Distance for real-time (near) updates.
    pub real_time_distance: i32,
    /// Whether the client wants real-time updates.
    pub real_time_updates: bool,
    /// Sync distance in chunks.
    pub sync_distance: i32,
    /// Whether sync is enabled client-side.
    pub sync_enabled: bool,
    /// Sync rate.
    pub sync_rate: i32,
}

/// Client messages accepted by the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientMessage {
    /// Session init for a dimension.
    Init {
        /// Dimension (level) name.
        dimension: String,
    },
    /// Negotiated client configuration.
    Config(SessionConfiguration),
    /// Request for one LOD section.
    Request {
        /// Client tracker id for this request.
        tracker: i32,
        /// Level (world) name.
        level: String,
        /// Requested section.
        section: Section,
        /// Optional last-modified timestamp of the client's cached copy.
        timestamp: Option<u64>,
    },
    /// Cancellation of a pending request.
    Cancel {
        /// Tracker id to cancel.
        tracker: i32,
    },
    /// Session close.
    Close,
}

/// Maximum size accepted for a single DH client message.
pub const MAX_CLIENT_MESSAGE_BYTES: usize = 4096;

/// Reads one client-to-server message and validates its complete layout.
///
/// # Errors
/// Returns a [`DecodeError`] when the payload is malformed, truncated, uses an
/// unsupported protocol or an unknown message id.
pub fn decode(data: &[u8]) -> Result<ClientMessage, DecodeError> {
    if data.len() > MAX_CLIENT_MESSAGE_BYTES {
        return Err(DecodeError::InvalidValue("message size"));
    }
    let mut input = Reader::new(data);
    let protocol = input.short()?;
    if protocol != PROTOCOL {
        return Err(DecodeError::IncompatibleProtocol(protocol));
    }
    let id = input.short()?;
    let message = match id {
        3 => ClientMessage::Init {
            dimension: input.string()?,
        },
        4 => {
            let generation_plan = input.byte()?;
            if generation_plan > 3 {
                return Err(DecodeError::InvalidValue("generator plan"));
            }
            let generation_distance = input.int()?;
            input.int()?;
            input.int()?;
            input.int()?;
            let generation_rate = input.int()?;
            let real_time_updates = input.bool()?;
            let real_time_distance = input.int()?;
            let sync_enabled = input.bool()?;
            let sync_distance = input.int()?;
            let sync_rate = input.int()?;
            let bandwidth_kbps = input.int()?;
            for v in [
                generation_distance,
                generation_rate,
                real_time_distance,
                sync_distance,
                sync_rate,
                bandwidth_kbps,
            ] {
                if v < 0 {
                    return Err(DecodeError::InvalidValue("client limits"));
                }
            }
            ClientMessage::Config(SessionConfiguration {
                bandwidth_kbps,
                generation_distance,
                generation_plan,
                generation_rate,
                real_time_distance,
                real_time_updates,
                sync_distance,
                sync_enabled,
                sync_rate,
            })
        }
        7 => {
            let tracker = input.int()?;
            let level = input.string()?;
            let section = read_section(&mut input)?;
            let timestamp = input.bool()?.then(|| input.timestamp()).transpose()?;
            ClientMessage::Request {
                tracker,
                level,
                section,
                timestamp,
            }
        }
        5 => ClientMessage::Cancel {
            tracker: input.int()?,
        },
        1 => {
            input.string()?;
            ClientMessage::Close
        }
        other => return Err(DecodeError::UnexpectedMessage(other)),
    };
    input.end()?;
    Ok(message)
}

/// Starts a server message with the protocol and message id.
fn packet(id: u16) -> Writer {
    let mut w = Writer::new();
    w.short(PROTOCOL).short(id);
    w
}

/// Announces the current world and the persistent server identity.
///
/// # Panics
/// Panics if `now` cannot be encoded as two 32-bit words; cannot happen for
/// unix timestamps in milliseconds.
#[must_use]
pub fn level_init(dimension: &str, server: &str, level: &str, now: u64) -> Vec<u8> {
    packet(2)
        .string(dimension)
        .string(server)
        .string(level)
        .timestamp(now)
        .expect("system timestamp fits in 64 bits")
        .finish()
}

/// Encodes the negotiated settings for one DH client session.
#[must_use]
pub fn session_config(config: SessionConfiguration) -> Vec<u8> {
    packet(4)
        .byte(config.generation_plan)
        .int(config.generation_distance)
        .int(0)
        .int(0)
        .int(0)
        .int(config.generation_rate)
        .bool(config.real_time_updates)
        .int(config.real_time_distance)
        .bool(config.sync_enabled)
        .int(config.sync_distance)
        .int(config.sync_rate)
        .int(config.bandwidth_kbps)
        .finish()
}

/// Rejects a request using the client's typed exception response.
#[must_use]
pub fn reject(tracker: i32, reason: &str, kind: i32) -> Vec<u8> {
    packet(6).int(tracker).int(kind).string(reason).finish()
}

/// Closes a DH session with a client-readable reason.
#[must_use]
pub fn close_session(reason: &str) -> Vec<u8> {
    packet(1).string(reason).finish()
}

/// Responds to an unchanged update-only request.
#[must_use]
pub fn unchanged(tracker: i32) -> Vec<u8> {
    packet(8).int(tracker).bool(false).finish()
}

/// Encodes one bounded transfer fragment.
///
/// # Panics
/// Never panics in practice; `offset` is clamped to the LOD length by callers.
#[must_use]
pub fn transfer_fragment(buffer: i32, lod: &[u8], offset: usize) -> Vec<u8> {
    let max_data = TRANSFER_PACKET_BYTES.min(lod.len().saturating_sub(offset));
    let part = &lod[offset..offset + max_data];
    packet(10).int(buffer).blob(part).bool(offset == 0).finish()
}

/// Encodes the response that completes a transfer after all fragments.
#[must_use]
pub fn transfer_response(tracker: i32, buffer: i32) -> Vec<u8> {
    packet(8).int(tracker).bool(true).int(buffer).int(0).finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SECTION_SIZE_BLOCKS;

    fn encoded_request(tracker: i32, with_ts: bool) -> Vec<u8> {
        let mut w = packet(7);
        w.int(tracker).string("world").words(0x1234_5678, 0x9abc_def0);
        if with_ts {
            w.bool(true).timestamp(1_700_000_000_000).unwrap();
        } else {
            w.bool(false);
        }
        w.finish()
    }

    #[test]
    fn decodes_request_with_timestamp() {
        let bytes = encoded_request(42, true);
        let ClientMessage::Request {
            tracker,
            level,
            timestamp,
            ..
        } = decode(&bytes).unwrap()
        else {
            panic!("expected request");
        };
        assert_eq!(tracker, 42);
        assert_eq!(level, "world");
        assert_eq!(timestamp, Some(1_700_000_000_000));
    }

    #[test]
    fn rejects_incompatible_protocol() {
        let mut w = Writer::new();
        w.short(15).short(1).string("x");
        assert_eq!(
            decode(&w.finish()),
            Err(DecodeError::IncompatibleProtocol(15))
        );
    }

    #[test]
    fn rejects_unknown_message() {
        let mut w = Writer::new();
        w.short(PROTOCOL).short(99);
        assert_eq!(
            decode(&w.finish()),
            Err(DecodeError::UnexpectedMessage(99))
        );
    }

    #[test]
    fn rejects_trailing_bytes() {
        let mut w = Writer::new();
        w.short(PROTOCOL).short(5).int(7).byte(1);
        assert_eq!(decode(&w.finish()), Err(DecodeError::UnexpectedEof));
    }

    #[test]
    fn section_roundtrip() {
        for &(x, z) in &[(0, 0), (1, -1), (-100, 1000), (12_000_000, -12_000_000)] {
            let section = Section::encode(x, z);
            assert_eq!(section.detail, SECTION_DETAIL);
            let mut w = Writer::new();
            w.words(section.high, section.low);
            let packed = w.finish();
            let mut r = Reader::new(&packed);
            let decoded = read_section(&mut r).unwrap();
            assert_eq!(decoded.x, x);
            assert_eq!(decoded.z, z);
            assert_eq!(decoded.detail, SECTION_DETAIL);
        }
    }

    #[test]
    fn transfer_fragments_are_bounded() {
        let lod = vec![0xab; TRANSFER_PACKET_BYTES * 2 + 123];
        let first = transfer_fragment(3, &lod, 0);
        let second = transfer_fragment(3, &lod, TRANSFER_PACKET_BYTES);
        let tail = transfer_fragment(3, &lod, TRANSFER_PACKET_BYTES * 2);

        let mut r = Reader::new(&first);
        r.short().unwrap();
        assert_eq!(r.short().unwrap(), 10);
        assert_eq!(r.int().unwrap(), 3);
        assert_eq!(r.blob().unwrap().len(), TRANSFER_PACKET_BYTES);
        assert!(r.bool().unwrap());
        let last = transfer_fragment(3, &lod, lod.len() - 1);
        let mut lr = Reader::new(&last);
        lr.short().unwrap();
        lr.short().unwrap();
        lr.int().unwrap();
        assert_eq!(lr.blob().map(<[u8]>::len), Ok(1));
        let _ = second;
        let _ = tail;
        let _ = SECTION_SIZE_BLOCKS;
    }

    #[test]
    fn server_messages_start_with_protocol() {
        assert_eq!(&level_init("dim", "srv", "lvl", 5)[..4], &[0, 16, 0, 2]);
        let config = SessionConfiguration {
            bandwidth_kbps: 500,
            generation_distance: 512,
            generation_plan: 0,
            generation_rate: 100,
            real_time_distance: 32,
            real_time_updates: true,
            sync_distance: 64,
            sync_enabled: true,
            sync_rate: 10,
        };
        assert_eq!(&session_config(config)[..4], &[0, 16, 0, 4]);
        assert_eq!(&reject(1, "no", 2)[..4], &[0, 16, 0, 6]);
        assert_eq!(&close_session("bye")[..4], &[0, 16, 0, 1]);
        assert_eq!(&unchanged(9)[..4], &[0, 16, 0, 8]);
        assert_eq!(&transfer_response(9, 3)[..4], &[0, 16, 0, 8]);
    }
}
