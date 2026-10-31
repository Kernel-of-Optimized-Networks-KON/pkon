//! The version-1 (`.pkon2`) container header: magic, 24-byte header layout, validation and CRC-32. Normative spec:
//! `docs/pkon2-format-v1.md`. All integers are little-endian and read byte by byte; nothing here depends on struct
//! layout or host endianness. Version 1 defined only codec ids 1..=3 (the retired PKON2 coder); no codec is registered
//! for it now, so `pkon::decode` reports `UnsupportedCodec` for every version-1 file after parsing its header.
use crate::canon::{Layout, Spec};
use std::fmt;

pub const MAGIC: [u8; 4] = [0x89, b'P', b'K', b'2'];
pub const VERSION: u8 = 1;
pub const HEADER_LEN: usize = 24;

/// Default resource policy: largest reconstructed pixel buffer a decode will allocate. Peak decode memory is about
/// 3-4x this (planes + two residual planes + output). Raise per call with `parse_header_with_limit`.
pub const DEFAULT_MAX_DECODED_BYTES: u64 = 1 << 28;

/// Every sample codes at least one binary decision whose cost is >= -log2(65520/65536) ~ 3.5e-4 bits (probability
/// clamp), i.e. >= 1 payload byte per ~22,700 samples. A header claiming more than 32,768 samples per payload byte
/// cannot be a valid stream and is rejected before any decoding work is done.
const MAX_SAMPLES_PER_PAYLOAD_BYTE: u64 = 32_768;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PkonError {
    /// Input ended before the header or the declared payload was complete.
    Truncated { need: usize, have: usize },
    BadMagic,
    UnsupportedVersion(u8),
    /// Non-zero flags byte (all flag bits are reserved and mandatory-to-understand in v1).
    ReservedFlags(u8),
    UnsupportedCodec(u8),
    UnsupportedPixelFormat(u8),
    /// Zero width or height.
    InvalidDimensions { width: u32, height: u32 },
    /// Reconstructed buffer would exceed the resource limit (or overflow arithmetic).
    TooLarge { width: u32, height: u32, limit: u64 },
    /// `payload_len` is impossible for the declared image.
    InvalidPayloadLength { payload_len: u32 },
    TrailingBytes(usize),
    /// The range decoder did not end exactly at the end of the payload.
    CorruptPayload,
    ChecksumMismatch { expected: u32, actual: u32 },
    /// Encoder input the format cannot represent (e.g. 16-bit samples).
    UnsupportedImage(String),
}

impl fmt::Display for PkonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use PkonError::*;
        match self {
            Truncated { need, have } => write!(f, "truncated: need {need} bytes, have {have}"),
            BadMagic => write!(f, "not a .pkon2 file (bad magic)"),
            UnsupportedVersion(v) => write!(f, "unsupported format version {v}"),
            ReservedFlags(b) => write!(f, "reserved flags set: {b:#04x}"),
            UnsupportedCodec(c) => write!(f, "unsupported codec id {c}"),
            UnsupportedPixelFormat(p) => write!(f, "unsupported pixel format {p}"),
            InvalidDimensions { width, height } => write!(f, "invalid dimensions {width}x{height}"),
            TooLarge { width, height, limit } => write!(f, "{width}x{height} image exceeds the {limit}-byte decode limit"),
            InvalidPayloadLength { payload_len } => write!(f, "payload length {payload_len} is impossible for the declared image"),
            TrailingBytes(n) => write!(f, "{n} unexpected trailing bytes after payload"),
            CorruptPayload => write!(f, "corrupt payload (range decoder did not end at payload end)"),
            ChecksumMismatch { expected, actual } => write!(f, "checksum mismatch: header {expected:#010x}, pixels {actual:#010x}"),
            UnsupportedImage(s) => write!(f, "unsupported image: {s}"),
        }
    }
}
impl std::error::Error for PkonError {}

/// Parsed and validated header (everything except the payload).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub spec: Spec,
    pub codec: u8,
    pub payload_len: u32,
    /// CRC-32 (IEEE) of the reconstructed pixel bytes.
    pub crc32: u32,
}

fn format_layout(code: u8) -> Option<Layout> {
    Some(match code {
        0 => Layout::Gray,
        1 => Layout::GrayAlpha,
        2 => Layout::Rgb,
        3 => Layout::Rgba,
        _ => return None,
    })
}

/// CRC-32/ISO-HDLC (zlib, PNG, Ethernet): poly 0xEDB88320 reflected, init and xorout 0xFFFFFFFF. Check("123456789") = 0xCBF43926.
/// Detects accidental corruption only; it is not authentication.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

fn rd32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// Parse and validate the 24-byte header with the default resource limit.
pub fn parse_header(bytes: &[u8]) -> Result<Header, PkonError> {
    parse_header_with_limit(bytes, DEFAULT_MAX_DECODED_BYTES)
}

/// Header validation only: no allocation, no payload access (payload presence is checked by `decode`).
pub fn parse_header_with_limit(bytes: &[u8], max_decoded_bytes: u64) -> Result<Header, PkonError> {
    if bytes.len() < HEADER_LEN {
        // A wrong magic is more useful than "truncated" when the prefix we do have already differs.
        let n = bytes.len().min(4);
        if bytes[..n] != MAGIC[..n] {
            return Err(PkonError::BadMagic);
        }
        return Err(PkonError::Truncated { need: HEADER_LEN, have: bytes.len() });
    }
    if bytes[0..4] != MAGIC {
        return Err(PkonError::BadMagic);
    }
    if bytes[4] != VERSION {
        return Err(PkonError::UnsupportedVersion(bytes[4]));
    }
    if bytes[5] != 0 {
        return Err(PkonError::ReservedFlags(bytes[5]));
    }
    let layout = format_layout(bytes[7]).ok_or(PkonError::UnsupportedPixelFormat(bytes[7]))?;
    let (width, height) = (rd32(bytes, 8), rd32(bytes, 12));
    let payload_len = rd32(bytes, 16);
    let crc32 = rd32(bytes, 20);
    if width == 0 || height == 0 {
        return Err(PkonError::InvalidDimensions { width, height });
    }
    let too_large = PkonError::TooLarge { width, height, limit: max_decoded_bytes };
    let samples = (width as u64).checked_mul(height as u64).and_then(|p| p.checked_mul(layout.channels() as u64)).ok_or(too_large.clone())?;
    // 8-bit samples only, so decoded bytes == samples. The limit must also be addressable on this platform.
    if samples > max_decoded_bytes || usize::try_from(samples).is_err() {
        return Err(too_large);
    }
    if payload_len == 0 || samples / MAX_SAMPLES_PER_PAYLOAD_BYTE > payload_len as u64 {
        return Err(PkonError::InvalidPayloadLength { payload_len });
    }
    Ok(Header { spec: Spec { width, height, layout, bit_depth: 8 }, codec: bytes[6], payload_len, crc32 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }
}
