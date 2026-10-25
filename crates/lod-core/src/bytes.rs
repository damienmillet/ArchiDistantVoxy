//! Big-endian byte reader/writer for the DH wire format.
//!
//! Adapted from DH Support. SPDX-License-Identifier: GPL-3.0-or-later.
#![allow(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::similar_names
)]

use std::fmt;

/// Errors produced while reading the DH wire format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// The payload ended before the message was complete.
    UnexpectedEof,
    /// The payload declared an invalid (negative or oversized) length field.
    InvalidLength,
    /// A value was outside its allowed range.
    InvalidValue(&'static str),
    /// The DH protocol version did not match the supported one.
    IncompatibleProtocol(u16),
    /// The message id is not a known client-to-server message.
    UnexpectedMessage(u16),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEof => write!(f, "DH payload ended unexpectedly"),
            Self::InvalidLength => write!(f, "DH payload declared an invalid length"),
            Self::InvalidValue(what) => write!(f, "DH payload has an invalid {what}"),
            Self::IncompatibleProtocol(p) => {
                write!(f, "incompatible DH protocol {p}, expected {PROTOCOL}")
            }
            Self::UnexpectedMessage(id) => write!(f, "unexpected DH client message {id}"),
        }
    }
}

impl std::error::Error for DecodeError {}

use crate::PROTOCOL;

/// Sequential big-endian reader over a DH payload.
#[derive(Debug)]
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    /// Reads `data` from the start.
    #[must_use]
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        let end = self.pos.checked_add(n).ok_or(DecodeError::InvalidLength)?;
        if end > self.data.len() {
            return Err(DecodeError::UnexpectedEof);
        }
        let slice = &self.data[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    /// Bytes consumed so far.
    #[must_use]
    pub fn position(&self) -> usize {
        self.pos
    }

    /// Remaining unread bytes.
    #[must_use]
    pub fn remaining(&self) -> &'a [u8] {
        &self.data[self.pos..]
    }

    /// Reads one byte.
    ///
    /// # Errors
    /// Fails when the payload is truncated or malformed.
    pub fn byte(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }

    /// Reads one byte as a boolean (nonzero = true).
    ///
    /// # Errors
    /// Fails when the payload is truncated or malformed.
    pub fn bool(&mut self) -> Result<bool, DecodeError> {
        Ok(self.byte()? != 0)
    }

    /// Reads a signed 16-bit big-endian integer.
    ///
    /// # Errors
    /// Fails when the payload is truncated or malformed.
    pub fn short(&mut self) -> Result<u16, DecodeError> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    /// Reads a signed 32-bit big-endian integer.
    ///
    /// # Errors
    /// Fails when the payload is truncated or malformed.
    pub fn int(&mut self) -> Result<i32, DecodeError> {
        let b = self.take(4)?;
        Ok(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Reads an exact 64-bit pattern as two 32-bit words, (high, low).
    ///
    /// # Errors
    /// Fails when the payload is truncated or malformed.
    pub fn words(&mut self) -> Result<(u32, u32), DecodeError> {
        let high = self.int()? as u32;
        let low = self.int()? as u32;
        Ok((high, low))
    }

    /// Reads a nonnegative 64-bit timestamp.
    ///
    /// # Errors
    /// Fails when the payload is truncated or malformed.
    pub fn timestamp(&mut self) -> Result<u64, DecodeError> {
        let (high, low) = self.words()?;
        Ok(u64::from(high) << 32 | u64::from(low))
    }

    /// Reads a length-prefixed (int, bytes) blob.
    ///
    /// # Errors
    /// Fails when the payload is truncated or malformed.
    pub fn blob(&mut self) -> Result<&'a [u8], DecodeError> {
        let len = self.int()?;
        if len < 0 || len as usize > self.data.len() {
            return Err(DecodeError::InvalidLength);
        }
        self.take(len as usize)
    }

    /// Reads a length-prefixed UTF-8 string.
    ///
    /// # Errors
    /// Fails when the payload is truncated or malformed.
    pub fn string(&mut self) -> Result<String, DecodeError> {
        let bytes = self.blob()?;
        String::from_utf8(bytes.to_vec()).map_err(|_| DecodeError::InvalidValue("string"))
    }

    /// Fails if the whole payload has not been consumed.
    ///
    /// # Errors
    /// Fails when the payload is truncated or malformed.
    pub fn end(&mut self) -> Result<(), DecodeError> {
        if self.pos == self.data.len() {
            Ok(())
        } else {
            Err(DecodeError::UnexpectedEof)
        }
    }
}

/// Incremental big-endian writer for the DH wire format.
#[derive(Debug, Default)]
pub struct Writer {
    data: Vec<u8>,
}

impl Writer {
    /// Creates an empty writer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends one byte.
    pub fn byte(&mut self, value: u8) -> &mut Self {
        self.data.push(value);
        self
    }

    /// Appends a boolean as 0/1.
    pub fn bool(&mut self, value: bool) -> &mut Self {
        self.byte(u8::from(value))
    }

    /// Appends a 16-bit integer.
    pub fn short(&mut self, value: u16) -> &mut Self {
        self.data.extend_from_slice(&value.to_be_bytes());
        self
    }

    /// Appends a 32-bit integer.
    pub fn int(&mut self, value: i32) -> &mut Self {
        self.data.extend_from_slice(&value.to_be_bytes());
        self
    }

    /// Appends an exact 64-bit pattern as two 32-bit words.
    pub fn words(&mut self, high: u32, low: u32) -> &mut Self {
        self.int(high as i32).int(low as i32)
    }

    /// Appends a nonnegative 64-bit timestamp.
    ///
    /// # Errors
    /// Fails if the timestamp exceeds 64 safe bits.
    pub fn timestamp(&mut self, value: u64) -> Result<&mut Self, DecodeError> {
        self.words((value >> 32) as u32, (value & u64::from(u32::MAX)) as u32);
        Ok(self)
    }

    /// Appends a length-prefixed blob.
    pub fn blob(&mut self, value: &[u8]) -> &mut Self {
        self.int(value.len() as i32);
        self.data.extend_from_slice(value);
        self
    }

    /// Appends a length-prefixed UTF-8 string.
    pub fn string(&mut self, value: &str) -> &mut Self {
        self.blob(value.as_bytes())
    }

    /// Consumes the writer and returns the encoded bytes.
    #[must_use]
    pub fn finish(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_primitives() {
        let mut w = Writer::new();
        w.short(16).int(-5).bool(true).string("hello").blob(&[1, 2]);
        w.timestamp(1_700_000_000_123).unwrap();
        let bytes = w.finish();

        let mut r = Reader::new(&bytes);
        assert_eq!(r.short().unwrap(), 16);
        assert_eq!(r.int().unwrap(), -5);
        assert!(r.bool().unwrap());
        assert_eq!(r.string().unwrap(), "hello");
        assert_eq!(r.blob().unwrap(), &[1, 2]);
        assert_eq!(r.timestamp().unwrap(), 1_700_000_000_123);
        r.end().unwrap();
    }

    #[test]
    fn eof_and_length_errors() {
        let mut r = Reader::new(&[0, 16, 0]);
        assert_eq!(r.short().unwrap(), 16);
        assert_eq!(r.short(), Err(DecodeError::UnexpectedEof));

        let mut r = Reader::new(&[0, 0, 0, 200]);
        assert_eq!(r.blob(), Err(DecodeError::InvalidLength));

        let mut r = Reader::new(&[1]);
        assert_eq!(r.remaining().len(), 1);
        assert_eq!(r.end(), Err(DecodeError::UnexpectedEof));
    }

    #[test]
    fn invalid_string_is_reported() {
        let mut w = Writer::new();
        w.blob(&[0xff, 0xfe, 0xfd]);
        let bytes = w.finish();
        let mut r = Reader::new(&bytes);
        assert_eq!(r.string(), Err(DecodeError::InvalidValue("string")));
    }
}
