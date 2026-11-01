//! The shared `.pkon` container, format version 2. Normative spec: `docs/pkon-format-v2.md`.
//!
//! One container, independent codecs. The container (this module) owns the outer structure: a fixed 32-byte header, a
//! codec-specific reconstruction-metadata block, the payload, and the integrity checks. A codec id selects the
//! compression algorithm and therefore how the metadata block and the payload are interpreted:
//!
//! * id 4: LISA RAW (payload produced by the unchanged LISA Stage 1..4 pipeline).
//! * id 6: ELLIE standard images (8/16-bit gray, gray+alpha, RGB, RGBA); one Stage 1..4 stream per plane, with an explicit
//!   colour representation (untransformed, or reversible YCoCg-R on RGB planes). Id 5 stays unassigned.
//!
//! Ids 1..=3 belonged to the retired PKON2 coder and are no longer supported; no standard-image codec is registered yet.
//! Version 1 files (`.pkon2`, `pkonfile`) share magic and version-byte position with version 2, so `decode` recognises both.
//! All integers are little-endian and read/written byte by byte; nothing depends on struct layout or host endianness.
use crate::canon::{Canonical, Layout, Spec};
use crate::ellie::{self, EllieCodec, Stage4Enc};
use crate::lisa::{self, CfaPattern, ColorFilter, ImageMetadata, LisaError, ReconstructionRecord, SampleLayout, Stage3Rule, Stage4Output};
use crate::lisa_bayer_plane::{self as bayer, Stage1Mode};
use crate::pkonfile::{self, PkonError};
use std::fmt;

pub use crate::pkonfile::{DEFAULT_MAX_DECODED_BYTES, MAGIC};

/// Container version written by this module.
pub const VERSION: u8 = 2;
/// Legacy container version (`pkonfile`): its header is still parsed by [`decode`], but it has no registered codec.
pub const VERSION_V1: u8 = 1;
pub const FIXED_HEADER_LEN: usize = 32;
/// Codec id 4: LISA RAW, production configuration (Stage 3 rule version 2 = OR-128, Stage 4 coding version 1).
pub const CODEC_LISA: u8 = 4;
/// Codec id 6: ELLIE (Stages 1-4 as in `ellie.rs`), per-plane streams, explicit colour representation in the metadata.
pub const CODEC_ELLIE: u8 = 6;
/// ELLIE representation ids (metadata byte 1). Planes are the interleaved channels, in order.
pub const ELLIE_REPR_UNTRANSFORMED: u8 = 0;
/// RGB planes 0..3 -> reversible YCoCg-R (Y 8-bit, Co/Cg 9-bit with +255 offset); an alpha plane is coded unchanged.
pub const ELLIE_REPR_YCOCG_R: u8 = 1;
/// Reconstruction-metadata layout version understood for every assigned codec so far.
pub const META_VERSION_1: u8 = 1;

/// LISA codec-metadata values accepted on disk (everything else is rejected, never guessed).
pub const LISA_STAGE3_RULE_OR128: u8 = lisa::STAGE3_RULE_VERSION;
pub const LISA_STAGE4_CODING_COUNTED: u8 = lisa::STAGE4_CODING_VERSION;

/// Upper bound on samples per payload byte, per codec. Every sample costs at least one binary decision, and a decision
/// costs at least -log2(p_max) bits where the probability clamp is [32, 65503]/65536 (`rangecoder::Bit::update`):
/// >= 7.3e-4 bits, i.e. <= ~11,000 samples per byte in theory. LISA uses a 16,384 bound (power-of-two margin).
/// Measured: a flat 2048x2048 image reaches 11,245 samples/byte (12-bit LISA).
const LISA_MAX_SAMPLES_PER_PAYLOAD_BYTE: u64 = 16_384;

// ---------------------------------------------------------------- errors

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContainerError {
    /// Error from the version-1 reader (`pkonfile`) or from the X4 payload decoder.
    V1(PkonError),
    Lisa(LisaError),
    Truncated { need: usize, have: usize },
    BadMagic,
    UnsupportedVersion(u8),
    /// Non-zero flags byte (all flag bits are reserved and mandatory-to-understand in v2).
    ReservedFlags(u8),
    UnsupportedCodec(u8),
    UnsupportedMetaVersion { codec: u8, version: u8 },
    /// A codec-metadata field names a variant this decoder does not implement.
    UnsupportedCodecVersion { codec: u8, field: &'static str, value: u8 },
    HeaderChecksumMismatch { expected: u32, actual: u32 },
    UnsupportedBitDepth { codec: u8, bit_depth: u8 },
    InvalidChannels { codec: u8, channels: u8 },
    InvalidDimensions { width: u32, height: u32 },
    TooLarge { width: u32, height: u32, limit: u64 },
    InvalidMetaLength { codec: u8, meta_len: usize },
    InvalidMetadata(String),
    InvalidPayloadLength { payload_len: u32 },
    TrailingBytes(usize),
    CorruptPayload(String),
    ChecksumMismatch { expected: u32, actual: u32 },
    /// Encoder input the format cannot represent.
    UnsupportedImage(String),
}

impl fmt::Display for ContainerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use ContainerError::*;
        match self {
            V1(e) => write!(f, "{e}"),
            Lisa(e) => write!(f, "LISA: {e}"),
            Truncated { need, have } => write!(f, "truncated: need {need} bytes, have {have}"),
            BadMagic => write!(f, "not a .pkon file (bad magic)"),
            UnsupportedVersion(v) => write!(f, "unsupported container version {v}"),
            ReservedFlags(b) => write!(f, "unknown mandatory feature flags set: {b:#04x}"),
            UnsupportedCodec(c) => write!(f, "unsupported codec id {c}"),
            UnsupportedMetaVersion { codec, version } => write!(f, "codec {codec}: unsupported metadata version {version}"),
            UnsupportedCodecVersion { codec, field, value } => write!(f, "codec {codec}: unsupported {field} {value}"),
            HeaderChecksumMismatch { expected, actual } => write!(f, "header checksum mismatch: stored {expected:#010x}, computed {actual:#010x}"),
            UnsupportedBitDepth { codec, bit_depth } => write!(f, "codec {codec}: unsupported bit depth {bit_depth}"),
            InvalidChannels { codec, channels } => write!(f, "codec {codec}: invalid channel count {channels}"),
            InvalidDimensions { width, height } => write!(f, "invalid dimensions {width}x{height}"),
            TooLarge { width, height, limit } => write!(f, "{width}x{height} image exceeds the {limit}-byte decode limit"),
            InvalidMetaLength { codec, meta_len } => write!(f, "codec {codec}: invalid metadata length {meta_len}"),
            InvalidMetadata(s) => write!(f, "invalid codec metadata: {s}"),
            InvalidPayloadLength { payload_len } => write!(f, "payload length {payload_len} is impossible for the declared image"),
            TrailingBytes(n) => write!(f, "{n} unexpected trailing bytes after payload"),
            CorruptPayload(s) => write!(f, "corrupt payload: {s}"),
            ChecksumMismatch { expected, actual } => write!(f, "pixel checksum mismatch: header {expected:#010x}, pixels {actual:#010x}"),
            UnsupportedImage(s) => write!(f, "unsupported image: {s}"),
        }
    }
}
impl std::error::Error for ContainerError {}

impl From<PkonError> for ContainerError {
    fn from(e: PkonError) -> Self {
        ContainerError::V1(e)
    }
}
impl From<LisaError> for ContainerError {
    fn from(e: LisaError) -> Self {
        match e {
            // A damaged Stage 4 stream or a residual that reconstructs outside the sample range is payload damage.
            LisaError::CorruptStream(s) => ContainerError::CorruptPayload(s.to_string()),
            LisaError::CorruptResidual { index } => ContainerError::CorruptPayload(format!("residual {index} reconstructs outside the sample range")),
            e => ContainerError::Lisa(e),
        }
    }
}

// ---------------------------------------------------------------- CRC-32 (table-driven; same function as `pkonfile::crc32`)

const CRC_TABLE: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

/// Incremental CRC-32/ISO-HDLC (zlib, PNG). Equals `pkonfile::crc32` over the concatenated input.
pub struct Crc32(u32);
impl Crc32 {
    pub fn new() -> Self {
        Crc32(!0)
    }
    pub fn update(&mut self, data: &[u8]) {
        for &b in data {
            self.0 = CRC_TABLE[((self.0 ^ b as u32) & 0xFF) as usize] ^ (self.0 >> 8);
        }
    }
    pub fn finish(&self) -> u32 {
        !self.0
    }
}
impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

/// Bytes per sample in the pixel-checksum byte stream and in the decoded-size accounting: 1 for `bit_depth <= 8`,
/// otherwise 2 (little-endian). For 8-bit standard images this is exactly the `canon::Canonical` byte layout.
pub fn bytes_per_sample(bit_depth: u8) -> usize {
    if bit_depth <= 8 { 1 } else { 2 }
}

/// CRC-32 of `samples` serialized row-major, one value per sample, `bytes_per_sample(bit_depth)` bytes each, little-endian.
pub fn pixel_crc_u16(samples: &[u16], bit_depth: u8) -> u32 {
    let mut crc = Crc32::new();
    let mut buf = [0u8; 4096];
    let wide = bytes_per_sample(bit_depth) == 2;
    for chunk in samples.chunks(if wide { 2048 } else { 4096 }) {
        let mut n = 0;
        for &s in chunk {
            buf[n] = s as u8;
            n += 1;
            if wide {
                buf[n] = (s >> 8) as u8;
                n += 1;
            }
        }
        crc.update(&buf[..n]);
    }
    crc.finish()
}

fn crc_of(parts: &[&[u8]]) -> u32 {
    let mut c = Crc32::new();
    for p in parts {
        c.update(p);
    }
    c.finish()
}

// ---------------------------------------------------------------- header

/// Codec-specific parameters, parsed and validated from the metadata block before any payload work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodecParams {
    Lisa(LisaParams),
    Ellie(EllieParams),
}

/// ELLIE metadata, version 1: `layout u8, representation u8`, then per plane `bit_depth u8, stream_len u32`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EllieParams {
    pub layout: Layout,
    pub representation: u8,
    /// `(coding bit depth, encoded stream length)` per plane, in channel order; lengths sum to the payload length.
    pub planes: Vec<(u8, u32)>,
}

/// Everything LISA's decoder needs besides dimensions, bit depth and payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LisaParams {
    pub stage1: Stage1Mode,
    /// Geometry comes from the container header; layout and CFA come from the metadata block.
    pub metadata: ImageMetadata,
    pub stage3_rule_version: u8,
    pub stage4_coding_version: u8,
}

/// Parsed and validated v2 header (everything except the payload).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub codec: u8,
    pub meta_version: u8,
    pub bit_depth: u8,
    pub channels: u8,
    pub width: u32,
    pub height: u32,
    pub meta_len: u16,
    pub payload_len: u32,
    pub pixel_crc32: u32,
    pub header_crc32: u32,
    pub params: CodecParams,
}

impl Header {
    /// Total file length implied by the header.
    pub fn file_len(&self) -> u64 {
        FIXED_HEADER_LEN as u64 + self.meta_len as u64 + self.payload_len as u64
    }
}

fn rd16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn rd32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

pub fn parse_header(bytes: &[u8]) -> Result<Header, ContainerError> {
    parse_header_with_limit(bytes, DEFAULT_MAX_DECODED_BYTES)
}

/// Header and codec-metadata validation only: no payload access and no allocation proportional to the image.
/// Order: magic -> version -> flags -> lengths -> header checksum -> codec -> metadata version -> bit depth/channels ->
/// dimensions -> decoded-size limit -> payload bounds -> codec metadata.
pub fn parse_header_with_limit(bytes: &[u8], max_decoded_bytes: u64) -> Result<Header, ContainerError> {
    let n = bytes.len().min(4);
    if bytes[..n] != MAGIC[..n] {
        return Err(ContainerError::BadMagic);
    }
    if bytes.len() >= 5 && bytes[4] != VERSION {
        return Err(ContainerError::UnsupportedVersion(bytes[4]));
    }
    if bytes.len() < FIXED_HEADER_LEN {
        return Err(ContainerError::Truncated { need: FIXED_HEADER_LEN, have: bytes.len() });
    }
    if bytes[5] != 0 {
        return Err(ContainerError::ReservedFlags(bytes[5]));
    }
    let meta_len = rd16(bytes, 10);
    let need = FIXED_HEADER_LEN + meta_len as usize;
    if bytes.len() < need {
        return Err(ContainerError::Truncated { need, have: bytes.len() });
    }
    let meta = &bytes[FIXED_HEADER_LEN..need];
    let (expected, actual) = (rd32(bytes, 28), crc_of(&[&bytes[..28], meta]));
    if expected != actual {
        return Err(ContainerError::HeaderChecksumMismatch { expected, actual });
    }

    let (codec, meta_version, bit_depth, channels) = (bytes[6], bytes[7], bytes[8], bytes[9]);
    let (width, height, payload_len, pixel_crc32) = (rd32(bytes, 12), rd32(bytes, 16), rd32(bytes, 20), rd32(bytes, 24));
    if codec != CODEC_LISA && codec != CODEC_ELLIE {
        return Err(ContainerError::UnsupportedCodec(codec));
    }
    let ellie = codec == CODEC_ELLIE;
    if meta_version != META_VERSION_1 {
        return Err(ContainerError::UnsupportedMetaVersion { codec, version: meta_version });
    }
    if !(if ellie { bit_depth == 8 || bit_depth == 16 } else { (1..=16).contains(&bit_depth) }) {
        return Err(ContainerError::UnsupportedBitDepth { codec, bit_depth });
    }
    if !(if ellie { (1..=4).contains(&channels) } else { channels == 1 }) {
        return Err(ContainerError::InvalidChannels { codec, channels });
    }
    let max_per_byte = LISA_MAX_SAMPLES_PER_PAYLOAD_BYTE;
    if width == 0 || height == 0 {
        return Err(ContainerError::InvalidDimensions { width, height });
    }
    let too_large = ContainerError::TooLarge { width, height, limit: max_decoded_bytes };
    let samples = (width as u64).checked_mul(height as u64).and_then(|p| p.checked_mul(channels as u64)).ok_or(too_large.clone())?;
    let decoded_bytes = samples.checked_mul(bytes_per_sample(bit_depth) as u64).ok_or(too_large.clone())?;
    if decoded_bytes > max_decoded_bytes || usize::try_from(decoded_bytes).is_err() {
        return Err(too_large);
    }
    // ELLIE has no samples-per-byte bound: the decoded-size limit above already caps its allocation.
    if payload_len == 0 || (!ellie && samples / max_per_byte > payload_len as u64) {
        return Err(ContainerError::InvalidPayloadLength { payload_len });
    }

    let params = if ellie {
        CodecParams::Ellie(parse_ellie_meta(meta, bit_depth, channels, payload_len)?)
    } else {
        CodecParams::Lisa(parse_lisa_meta(meta, width, height, bit_depth)?)
    };
    Ok(Header { codec, meta_version, bit_depth, channels, width, height, meta_len, payload_len, pixel_crc32, header_crc32: expected, params })
}

// ---------------------------------------------------------------- LISA reconstruction metadata, version 1

const L_STAGE1_MOSAIC: u8 = 0;
const L_STAGE1_BAYER_PLANAR: u8 = 1;
const LISA_META_FIXED: usize = 4;
const LISA_CFA_FIXED: usize = 8;

fn layout_code(l: SampleLayout) -> u8 {
    match l {
        SampleLayout::Unspecified => 0,
        SampleLayout::Monochrome => 1,
        SampleLayout::Linear => 2,
        SampleLayout::BayerCfa => 3,
        SampleLayout::OtherCfa => 4,
    }
}

fn layout_from_code(c: u8) -> Option<SampleLayout> {
    Some(match c {
        0 => SampleLayout::Unspecified,
        1 => SampleLayout::Monochrome,
        2 => SampleLayout::Linear,
        3 => SampleLayout::BayerCfa,
        4 => SampleLayout::OtherCfa,
        _ => return None,
    })
}

fn has_cfa(l: SampleLayout) -> bool {
    matches!(l, SampleLayout::BayerCfa | SampleLayout::OtherCfa)
}

fn parse_lisa_meta(meta: &[u8], width: u32, height: u32, bit_depth: u8) -> Result<LisaParams, ContainerError> {
    let bad_len = || ContainerError::InvalidMetaLength { codec: CODEC_LISA, meta_len: meta.len() };
    if meta.len() < LISA_META_FIXED {
        return Err(bad_len());
    }
    let unsupported = |field, value| ContainerError::UnsupportedCodecVersion { codec: CODEC_LISA, field, value };
    let stage1 = match meta[0] {
        L_STAGE1_MOSAIC => Stage1Mode::Mosaic,
        L_STAGE1_BAYER_PLANAR => Stage1Mode::BayerPlanar,
        v => return Err(unsupported("stage1_mode", v)),
    };
    let layout = layout_from_code(meta[1]).ok_or_else(|| unsupported("sample_layout", meta[1]))?;
    if meta[2] != LISA_STAGE3_RULE_OR128 {
        return Err(unsupported("stage3_rule_version", meta[2]));
    }
    if meta[3] != LISA_STAGE4_CODING_COUNTED {
        return Err(unsupported("stage4_coding_version", meta[3]));
    }
    let cfa = if has_cfa(layout) {
        if meta.len() < LISA_META_FIXED + LISA_CFA_FIXED {
            return Err(bad_len());
        }
        let (cw, ch, ox, oy) = (rd16(meta, 4), rd16(meta, 6), rd16(meta, 8), rd16(meta, 10));
        let entries = cw as usize * ch as usize;
        if meta.len() != LISA_META_FIXED + LISA_CFA_FIXED + entries {
            return Err(bad_len());
        }
        let entries = meta[LISA_META_FIXED + LISA_CFA_FIXED..].iter().map(|&c| ColorFilter::try_from(c)).collect::<Result<Vec<_>, _>>()?;
        Some(CfaPattern { width: cw as usize, height: ch as usize, entries, origin_x: ox as usize, origin_y: oy as usize })
    } else {
        if meta.len() != LISA_META_FIXED {
            return Err(bad_len());
        }
        None
    };
    let metadata = ImageMetadata { width: width as usize, height: height as usize, layout, cfa };
    // Structural validation (CFA dimensions, origin range, Bayer arrangement) and the geometry/bit-depth limits.
    metadata.validate()?;
    lisa::validate(metadata.width, metadata.height, bit_depth)?;
    if stage1 == Stage1Mode::BayerPlanar && layout != SampleLayout::BayerCfa {
        return Err(ContainerError::InvalidMetadata("planar Stage 1 requires a Bayer CFA layout".into()));
    }
    Ok(LisaParams { stage1, metadata, stage3_rule_version: meta[2], stage4_coding_version: meta[3] })
}

fn serialize_lisa_meta(p: &LisaParams) -> Result<Vec<u8>, ContainerError> {
    let unsupported = |field, value| ContainerError::UnsupportedCodecVersion { codec: CODEC_LISA, field, value };
    if p.stage3_rule_version != LISA_STAGE3_RULE_OR128 {
        return Err(unsupported("stage3_rule_version", p.stage3_rule_version));
    }
    if p.stage4_coding_version != LISA_STAGE4_CODING_COUNTED {
        return Err(unsupported("stage4_coding_version", p.stage4_coding_version));
    }
    let mut m = vec![
        match p.stage1 {
            Stage1Mode::Mosaic => L_STAGE1_MOSAIC,
            Stage1Mode::BayerPlanar => L_STAGE1_BAYER_PLANAR,
        },
        layout_code(p.metadata.layout),
        p.stage3_rule_version,
        p.stage4_coding_version,
    ];
    if let Some(cfa) = &p.metadata.cfa {
        let fit = |v: usize, what: &str| u16::try_from(v).map_err(|_| ContainerError::UnsupportedImage(format!("CFA {what} {v} exceeds the 16-bit on-disk field")));
        for v in [fit(cfa.width, "width")?, fit(cfa.height, "height")?, fit(cfa.origin_x, "origin_x")?, fit(cfa.origin_y, "origin_y")?] {
            m.extend_from_slice(&v.to_le_bytes());
        }
        m.extend(cfa.entries.iter().map(|&e| e as u8));
    }
    if m.len() > u16::MAX as usize {
        return Err(ContainerError::UnsupportedImage(format!("codec metadata of {} bytes exceeds the 65535-byte limit", m.len())));
    }
    Ok(m)
}

// ---------------------------------------------------------------- ELLIE metadata, version 1

const ELLIE_META_FIXED: usize = 2;
const ELLIE_PLANE_META: usize = 5;

fn ellie_layout_code(l: Layout) -> u8 {
    match l {
        Layout::Gray => 0,
        Layout::GrayAlpha => 1,
        Layout::Rgb => 2,
        Layout::Rgba => 3,
    }
}

fn ellie_layout_from_code(c: u8) -> Option<Layout> {
    Some(match c {
        0 => Layout::Gray,
        1 => Layout::GrayAlpha,
        2 => Layout::Rgb,
        3 => Layout::Rgba,
        _ => return None,
    })
}

/// Coding bit depth of plane `k` for a representation at sample depth `bit_depth` (None: representation not valid here).
fn ellie_plane_bits(repr: u8, layout: Layout, bit_depth: u8, k: usize) -> Option<u8> {
    match repr {
        ELLIE_REPR_UNTRANSFORMED => Some(bit_depth),
        ELLIE_REPR_YCOCG_R if bit_depth == 8 && matches!(layout, Layout::Rgb | Layout::Rgba) => {
            Some(if k == 1 || k == 2 { ellie::YCOCG_CHROMA_BITS as u8 } else { 8 })
        }
        _ => None,
    }
}

fn parse_ellie_meta(meta: &[u8], bit_depth: u8, channels: u8, payload_len: u32) -> Result<EllieParams, ContainerError> {
    let n = channels as usize;
    if meta.len() != ELLIE_META_FIXED + ELLIE_PLANE_META * n {
        return Err(ContainerError::InvalidMetaLength { codec: CODEC_ELLIE, meta_len: meta.len() });
    }
    let unsupported = |field, value| ContainerError::UnsupportedCodecVersion { codec: CODEC_ELLIE, field, value };
    let layout = ellie_layout_from_code(meta[0]).ok_or_else(|| unsupported("sample_layout", meta[0]))?;
    if layout.channels() != n {
        return Err(ContainerError::InvalidMetadata("layout disagrees with the channel count".into()));
    }
    let representation = meta[1];
    if !matches!(representation, ELLIE_REPR_UNTRANSFORMED | ELLIE_REPR_YCOCG_R) {
        return Err(unsupported("representation", representation));
    }
    let mut planes = Vec::with_capacity(n);
    let mut total = 0u64;
    for k in 0..n {
        let at = ELLIE_META_FIXED + ELLIE_PLANE_META * k;
        let (bits, len) = (meta[at], rd32(meta, at + 1));
        match ellie_plane_bits(representation, layout, bit_depth, k) {
            Some(b) if b == bits => {}
            Some(_) => return Err(ContainerError::InvalidMetadata(format!("plane {k} bit depth {bits} does not match the representation"))),
            None => return Err(unsupported("representation", representation)),
        }
        if len == 0 {
            return Err(ContainerError::InvalidMetadata(format!("plane {k} has an empty stream")));
        }
        total += len as u64;
        planes.push((bits, len));
    }
    if total != payload_len as u64 {
        return Err(ContainerError::InvalidMetadata("plane stream lengths do not sum to the payload length".into()));
    }
    Ok(EllieParams { layout, representation, planes })
}

fn serialize_ellie_meta(p: &EllieParams) -> Vec<u8> {
    let mut m = vec![ellie_layout_code(p.layout), p.representation];
    for &(bits, len) in &p.planes {
        m.push(bits);
        m.extend_from_slice(&len.to_le_bytes());
    }
    m
}

// ---------------------------------------------------------------- writing

struct FileParts<'a> {
    codec: u8,
    bit_depth: u8,
    channels: u8,
    width: u32,
    height: u32,
    meta: &'a [u8],
    payload: &'a [u8],
    pixel_crc32: u32,
}

fn write_file(p: FileParts) -> Result<Vec<u8>, ContainerError> {
    let meta_len = u16::try_from(p.meta.len()).map_err(|_| ContainerError::UnsupportedImage("codec metadata exceeds 65535 bytes".into()))?;
    let payload_len = u32::try_from(p.payload.len()).map_err(|_| ContainerError::UnsupportedImage("payload exceeds 4 GiB".into()))?;
    let mut out = Vec::with_capacity(FIXED_HEADER_LEN + p.meta.len() + p.payload.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&[VERSION, 0, p.codec, META_VERSION_1, p.bit_depth, p.channels]);
    out.extend_from_slice(&meta_len.to_le_bytes());
    out.extend_from_slice(&p.width.to_le_bytes());
    out.extend_from_slice(&p.height.to_le_bytes());
    out.extend_from_slice(&payload_len.to_le_bytes());
    out.extend_from_slice(&p.pixel_crc32.to_le_bytes());
    let header_crc = crc_of(&[&out, p.meta]);
    out.extend_from_slice(&header_crc.to_le_bytes());
    out.extend_from_slice(p.meta);
    out.extend_from_slice(p.payload);
    // Fail loudly if our own output would not pass our own header validation.
    parse_header_with_limit(&out, u64::MAX)?;
    Ok(out)
}

/// Which colour representation the ELLIE encoder uses for a standard image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EllieRepr {
    /// Try every valid representation and keep the smaller complete file (ties go to untransformed). Equivalent to
    /// `Untransformed` for gray, gray+alpha and 16-bit input.
    Auto,
    Untransformed,
    /// Reversible YCoCg-R on the RGB planes of 8-bit RGB/RGBA input; an error for anything else.
    YCoCgR,
}

/// Encode a decoded-pixel standard image with ELLIE (codec 6). 8-bit RGB/RGBA input is tried both untransformed and as
/// YCoCg-R, and the smaller file is written.
pub fn encode_standard(img: &Canonical) -> Result<Vec<u8>, ContainerError> {
    encode_standard_with(img, EllieRepr::Auto)
}

pub fn encode_standard_with(img: &Canonical, repr: EllieRepr) -> Result<Vec<u8>, ContainerError> {
    let rgb8 = img.spec.bit_depth == 8 && matches!(img.spec.layout, Layout::Rgb | Layout::Rgba);
    match repr {
        EllieRepr::Untransformed => encode_ellie(img, ELLIE_REPR_UNTRANSFORMED),
        EllieRepr::YCoCgR if rgb8 => encode_ellie(img, ELLIE_REPR_YCOCG_R),
        EllieRepr::YCoCgR => Err(ContainerError::UnsupportedImage("YCoCg-R needs 8-bit RGB or RGBA input".into())),
        EllieRepr::Auto => {
            let plain = encode_ellie(img, ELLIE_REPR_UNTRANSFORMED)?;
            if !rgb8 {
                return Ok(plain);
            }
            let ycocg = encode_ellie(img, ELLIE_REPR_YCOCG_R)?;
            Ok(if ycocg.len() < plain.len() { ycocg } else { plain })
        }
    }
}

fn ellie_err(e: ellie::EllieError) -> ContainerError {
    ContainerError::CorruptPayload(format!("ELLIE: {e:?}"))
}

fn encode_ellie(img: &Canonical, repr: u8) -> Result<Vec<u8>, ContainerError> {
    let Spec { width, height, layout, bit_depth } = img.spec;
    let (w, h, ch) = (width as usize, height as usize, layout.channels());
    let n = w * h;
    let mut planes = vec![vec![0u16; n]; ch];
    if bit_depth == 8 {
        for (i, px) in img.data.chunks_exact(ch).enumerate() {
            for (k, &v) in px.iter().enumerate() {
                planes[k][i] = v as u16;
            }
        }
    } else {
        for (i, px) in img.data.chunks_exact(2 * ch).enumerate() {
            for k in 0..ch {
                planes[k][i] = u16::from_le_bytes([px[2 * k], px[2 * k + 1]]);
            }
        }
    }
    if repr == ELLIE_REPR_YCOCG_R && !ellie::ycocg_r_forward_planes(&mut planes[..3]) {
        return Err(ContainerError::UnsupportedImage("YCoCg-R input out of range".into()));
    }
    let mut payload = Vec::new();
    let mut meta_planes = Vec::with_capacity(ch);
    let mut enc = Stage4Enc::with_capacity(n);
    let mut codecs: Vec<(u8, EllieCodec)> = vec![];
    for (k, plane) in planes.iter().enumerate() {
        let bits = ellie_plane_bits(repr, layout, bit_depth, k).ok_or_else(|| ContainerError::UnsupportedImage("invalid ELLIE representation".into()))?;
        let at = match codecs.iter().position(|c| c.0 == bits) {
            Some(i) => i,
            None => {
                codecs.push((bits, EllieCodec::new(w, h, bits as u32).map_err(ellie_err)?));
                codecs.len() - 1
            }
        };
        enc.reset();
        codecs[at].1.encode_planes(&[plane.as_slice()], &mut enc).map_err(ellie_err)?;
        let bytes = enc.finish();
        let len = u32::try_from(bytes.len()).map_err(|_| ContainerError::UnsupportedImage("plane stream exceeds 4 GiB".into()))?;
        payload.extend_from_slice(bytes);
        meta_planes.push((bits, len));
    }
    let meta = serialize_ellie_meta(&EllieParams { layout, representation: repr, planes: meta_planes });
    let mut crc = Crc32::new();
    crc.update(&img.data);
    write_file(FileParts {
        codec: CODEC_ELLIE,
        bit_depth,
        channels: ch as u8,
        width,
        height,
        meta: &meta,
        payload: &payload,
        pixel_crc32: crc.finish(),
    })
}

fn decode_ellie(hdr: &Header, p: &EllieParams, payload: &[u8]) -> Result<Canonical, ContainerError> {
    let (w, h, ch) = (hdr.width as usize, hdr.height as usize, hdr.channels as usize);
    let n = w * h;
    let mut planes = vec![vec![0u16; n]; ch];
    let mut codecs: Vec<(u8, EllieCodec)> = vec![];
    let mut at = 0usize;
    for (k, &(bits, len)) in p.planes.iter().enumerate() {
        let stream = &payload[at..at + len as usize];
        at += len as usize;
        let i = match codecs.iter().position(|c| c.0 == bits) {
            Some(i) => i,
            None => {
                codecs.push((bits, EllieCodec::new(w, h, bits as u32).map_err(ellie_err)?));
                codecs.len() - 1
            }
        };
        codecs[i].1.decode_planes(stream, &mut [planes[k].as_mut_slice()]).map_err(ellie_err)?;
    }
    if p.representation == ELLIE_REPR_YCOCG_R && !ellie::ycocg_r_inverse_planes(&mut planes[..3]) {
        return Err(ContainerError::CorruptPayload("YCoCg-R plane value outside its domain".into()));
    }
    let bps = bytes_per_sample(hdr.bit_depth);
    let mut data = vec![0u8; n * ch * bps];
    for (k, plane) in planes.iter().enumerate() {
        for (i, &v) in plane.iter().enumerate() {
            let o = (i * ch + k) * bps;
            data[o] = v as u8;
            if bps == 2 {
                data[o + 1] = (v >> 8) as u8;
            }
        }
    }
    let mut crc = Crc32::new();
    crc.update(&data);
    let actual = crc.finish();
    if actual != hdr.pixel_crc32 {
        return Err(ContainerError::ChecksumMismatch { expected: hdr.pixel_crc32, actual });
    }
    let spec = Spec { width: hdr.width, height: hdr.height, layout: p.layout, bit_depth: hdr.bit_depth };
    Canonical::new(spec, data).map_err(ContainerError::InvalidMetadata)
}

/// Encode a RAW mosaic or plane with LISA (production path: per-plane Stage 1 for Bayer CFA, OR-128, Stage 4 counted).
/// `samples` is row-major `u16`, each below `1 << bit_depth`; `metadata` carries geometry, layout and CFA.
pub fn encode_raw(samples: &[u16], width: usize, height: usize, bit_depth: u8, metadata: ImageMetadata) -> Result<Vec<u8>, ContainerError> {
    encode_raw_with_mode(samples, width, height, bit_depth, metadata, Stage1Mode::default())
}

/// [`encode_raw`] with an explicit Stage 1 mode (`Mosaic` is the supported fallback for Bayer layouts).
pub fn encode_raw_with_mode(
    samples: &[u16],
    width: usize,
    height: usize,
    bit_depth: u8,
    metadata: ImageMetadata,
    mode: Stage1Mode,
) -> Result<Vec<u8>, ContainerError> {
    let (w32, h32) = (u32::try_from(width), u32::try_from(height));
    let (Ok(w32), Ok(h32)) = (w32, h32) else {
        return Err(LisaError::DimensionTooLarge.into());
    };
    let s2 = bayer::encode_stage2_with_mode(samples, width, height, bit_depth, metadata, mode)?;
    let s4 = lisa::encode_stage4(lisa::classify_stage3(s2)?)?;
    let params = lisa_params_of(&s4)?;
    let meta = serialize_lisa_meta(&params)?;
    write_file(FileParts {
        codec: CODEC_LISA,
        bit_depth,
        channels: 1,
        width: w32,
        height: h32,
        meta: &meta,
        payload: s4.payload(),
        pixel_crc32: pixel_crc_u16(samples, bit_depth),
    })
}

/// The reconstruction parameters held by a `Stage4Output` history (Stage 1 mode, Stage 3 rule, Stage 4 coding version).
fn lisa_params_of(s4: &Stage4Output) -> Result<LisaParams, ContainerError> {
    let [s1, ReconstructionRecord::Stage2Organization(_), ReconstructionRecord::Stage3Context(r3), ReconstructionRecord::Stage4Entropy(r4)] = s4.history() else {
        return Err(ContainerError::InvalidMetadata("unexpected Stage 4 reconstruction history".into()));
    };
    let stage1 = match s1 {
        ReconstructionRecord::Stage1BayerPlanar { .. } => Stage1Mode::BayerPlanar,
        ReconstructionRecord::Stage1Decorrelation { .. } => Stage1Mode::Mosaic,
        _ => return Err(ContainerError::InvalidMetadata("unrecognised Stage 1 record".into())),
    };
    Ok(LisaParams { stage1, metadata: s4.metadata().clone(), stage3_rule_version: r3.rule_version, stage4_coding_version: r4.coding_version })
}

// ---------------------------------------------------------------- decoding

/// A decoded RAW image: row-major `u16` samples plus the metadata (geometry, layout, CFA) needed to interpret them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawImage {
    pub samples: Vec<u16>,
    pub metadata: ImageMetadata,
    pub bit_depth: u8,
}

/// Result of decoding any supported `.pkon` / `.pkon2` file.
#[derive(Clone, Debug)]
pub enum Decoded {
    /// Decoded-pixel standard image (ELLIE, codec 6).
    Standard(Canonical),
    /// LISA RAW image (container v2, codec 4).
    Raw(RawImage),
}

/// Container version of a file (`1` = `.pkon2` layout, `2` = this module). Checks the magic only.
pub fn container_version(bytes: &[u8]) -> Result<u8, ContainerError> {
    let n = bytes.len().min(4);
    if bytes[..n] != MAGIC[..n] {
        return Err(ContainerError::BadMagic);
    }
    bytes.get(4).copied().ok_or(ContainerError::Truncated { need: 5, have: bytes.len() })
}

pub fn decode(bytes: &[u8]) -> Result<Decoded, ContainerError> {
    decode_with_limit(bytes, DEFAULT_MAX_DECODED_BYTES)
}

/// Decode any supported file. Version 1 is recognised but has no registered codec; version 2 is read here.
/// `max_decoded_bytes` caps the reconstructed sample buffer. LISA's transient working set is about 16 bytes per sample on
/// top of that, so raise the limit with the available memory in mind.
pub fn decode_with_limit(bytes: &[u8], max_decoded_bytes: u64) -> Result<Decoded, ContainerError> {
    match container_version(bytes)? {
        // Version 1 defined only the retired PKON2 codecs: a well-formed header is reported as an unsupported codec.
        VERSION_V1 => Err(match pkonfile::parse_header_with_limit(bytes, max_decoded_bytes) {
            Err(e) => e,
            Ok(h) => PkonError::UnsupportedCodec(h.codec),
        }
        .into()),
        VERSION => decode_v2(bytes, max_decoded_bytes),
        v => Err(ContainerError::UnsupportedVersion(v)),
    }
}

fn decode_v2(bytes: &[u8], max_decoded_bytes: u64) -> Result<Decoded, ContainerError> {
    let hdr = parse_header_with_limit(bytes, max_decoded_bytes)?;
    let end = usize::try_from(hdr.file_len()).map_err(|_| ContainerError::InvalidPayloadLength { payload_len: hdr.payload_len })?;
    if bytes.len() < end {
        return Err(ContainerError::Truncated { need: end, have: bytes.len() });
    }
    if bytes.len() > end {
        return Err(ContainerError::TrailingBytes(bytes.len() - end));
    }
    let payload = &bytes[FIXED_HEADER_LEN + hdr.meta_len as usize..end];
    let check = |actual: u32| {
        if actual == hdr.pixel_crc32 { Ok(()) } else { Err(ContainerError::ChecksumMismatch { expected: hdr.pixel_crc32, actual }) }
    };
    match &hdr.params {
        CodecParams::Ellie(p) => Ok(Decoded::Standard(decode_ellie(&hdr, p, payload)?)),
        CodecParams::Lisa(p) => {
            let s4 = Stage4Output::from_parts(
                payload.to_vec(),
                p.metadata.clone(),
                hdr.bit_depth,
                p.stage1 == Stage1Mode::BayerPlanar,
                Stage3Rule::Or128,
                p.stage4_coding_version,
            )?;
            let d3 = lisa::decode_stage4(&s4)?;
            let (s1, mode) = bayer::restore_stage1_with_mode(&d3.into_stage2())?;
            if mode != p.stage1 {
                return Err(ContainerError::InvalidMetadata("Stage 1 mode disagrees with the stream".into()));
            }
            let samples = bayer::decode_stage1(&s1, mode)?;
            check(pixel_crc_u16(&samples, hdr.bit_depth))?;
            Ok(Decoded::Raw(RawImage { samples, metadata: p.metadata.clone(), bit_depth: hdr.bit_depth }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_matches_check_value_and_v1_function() {
        let mut c = Crc32::new();
        c.update(b"1234");
        c.update(b"56789");
        assert_eq!(c.finish(), 0xCBF4_3926);
        let data: Vec<u8> = (0..1000u32).map(|i| (i * 7) as u8).collect();
        let mut c = Crc32::new();
        c.update(&data);
        assert_eq!(c.finish(), pkonfile::crc32(&data));
    }

    #[test]
    fn pixel_crc_is_little_endian_bytes() {
        let s = [0x0102u16, 0x0304, 0x0fff];
        assert_eq!(pixel_crc_u16(&s, 12), pkonfile::crc32(&[2, 1, 4, 3, 0xff, 0x0f]));
        let s8 = [1u16, 2, 255];
        assert_eq!(pixel_crc_u16(&s8, 8), pkonfile::crc32(&[1, 2, 255]));
    }
}
