//! Candidate-codec interface plus the in-process reference codecs.
use crate::canon::{Canonical, Layout, Spec};
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{ExtendedColorType, ImageEncoder, ImageFormat};

#[derive(Debug, Clone)]
pub enum CodecError {
    /// This codec cannot represent this image losslessly (reported, never converted around).
    Unsupported(String),
    Failed(String),
}

impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodecError::Unsupported(s) => write!(f, "unsupported: {s}"),
            CodecError::Failed(s) => write!(f, "failed: {s}"),
        }
    }
}

/// A lossless image codec under test.
///
/// `decode` receives only the encoded bytes plus `hint`, the expected format/size. The hint
/// stands in for "out-of-band knowledge the caller has" and must never carry pixel data.
/// Codecs that need dimensions must put them in their own stream (so their bytes are counted).
pub trait Codec {
    fn name(&self) -> String;
    fn version(&self) -> String;
    fn config(&self) -> String;
    /// `Err(reason)` means the codec cannot run here at all (tool missing); that is
    /// "unavailable", distinct from a codec that runs and fails.
    fn available(&self) -> Result<(), String> {
        Ok(())
    }
    fn supports(&self, _spec: &Spec) -> bool {
        true
    }
    fn encode(&self, img: &Canonical) -> Result<Vec<u8>, CodecError>;
    fn decode(&self, encoded: &[u8], hint: &Spec) -> Result<Canonical, CodecError>;
    /// Bytes of `encoded` that are container/metadata rather than pixel payload, if known.
    fn container_overhead(&self, _encoded: &[u8]) -> Option<u64> {
        None
    }
}

/// Identity codec with a 10-byte self-describing header. Not a competitor: it checks that the
/// harness itself accounts honestly (encoded = canonical + 10, ratio just under 1).
pub struct RawCodec;

const RAW_HEADER: usize = 10;

impl Codec for RawCodec {
    fn name(&self) -> String {
        "raw".into()
    }
    fn version(&self) -> String {
        "1".into()
    }
    fn config(&self) -> String {
        "w:u32le h:u32le layout:u8 depth:u8 + samples".into()
    }
    fn encode(&self, img: &Canonical) -> Result<Vec<u8>, CodecError> {
        let mut out = Vec::with_capacity(RAW_HEADER + img.data.len());
        out.extend(img.spec.width.to_le_bytes());
        out.extend(img.spec.height.to_le_bytes());
        out.push(img.spec.layout.channels() as u8 | ((img.spec.layout.has_alpha() as u8) << 4));
        out.push(img.spec.bit_depth);
        out.extend(&img.data);
        Ok(out)
    }
    fn decode(&self, enc: &[u8], _hint: &Spec) -> Result<Canonical, CodecError> {
        if enc.len() < RAW_HEADER {
            return Err(CodecError::Failed("truncated header".into()));
        }
        let width = u32::from_le_bytes(enc[0..4].try_into().unwrap());
        let height = u32::from_le_bytes(enc[4..8].try_into().unwrap());
        let layout = match enc[8] {
            0x01 => Layout::Gray,
            0x12 => Layout::GrayAlpha,
            0x03 => Layout::Rgb,
            0x14 => Layout::Rgba,
            b => return Err(CodecError::Failed(format!("bad layout byte {b:#x}"))),
        };
        let spec = Spec { width, height, layout, bit_depth: enc[9] };
        // Validate before allocating anything: header fields are untrusted.
        let want = (width as u64)
            .saturating_mul(height as u64)
            .saturating_mul(layout.channels() as u64)
            .saturating_mul((spec.bit_depth / 8) as u64);
        if want != (enc.len() - RAW_HEADER) as u64 {
            return Err(CodecError::Failed(format!("payload {} bytes, header implies {want}", enc.len() - RAW_HEADER)));
        }
        Canonical::new(spec, enc[RAW_HEADER..].to_vec()).map_err(CodecError::Failed)
    }
    fn container_overhead(&self, _enc: &[u8]) -> Option<u64> {
        Some(RAW_HEADER as u64)
    }
}

/// PNG via the `image` crate's encoder (Rust, in-process).
pub struct PngCodec;

impl Codec for PngCodec {
    fn name(&self) -> String {
        "png-rs".into()
    }
    fn version(&self) -> String {
        format!("image {} / png {}", env!("NS_IMAGE_VERSION"), env!("NS_PNG_VERSION"))
    }
    fn config(&self) -> String {
        "image::PngEncoder CompressionType::Best, FilterType::Adaptive, no metadata".into()
    }
    fn encode(&self, img: &Canonical) -> Result<Vec<u8>, CodecError> {
        let color = match (img.spec.layout, img.spec.bit_depth) {
            (Layout::Gray, 8) => ExtendedColorType::L8,
            (Layout::GrayAlpha, 8) => ExtendedColorType::La8,
            (Layout::Rgb, 8) => ExtendedColorType::Rgb8,
            (Layout::Rgba, 8) => ExtendedColorType::Rgba8,
            (Layout::Gray, 16) => ExtendedColorType::L16,
            (Layout::GrayAlpha, 16) => ExtendedColorType::La16,
            (Layout::Rgb, 16) => ExtendedColorType::Rgb16,
            (Layout::Rgba, 16) => ExtendedColorType::Rgba16,
            _ => return Err(CodecError::Unsupported(img.spec.label())),
        };
        // The image crate wants 16-bit samples in native endianness; canonical is LE.
        let native: Vec<u8>;
        let bytes: &[u8] = if img.spec.bit_depth == 16 {
            native = img.data.chunks(2).flat_map(|c| u16::from_le_bytes([c[0], c[1]]).to_ne_bytes()).collect();
            &native
        } else {
            &img.data
        };
        let mut out = vec![];
        PngEncoder::new_with_quality(&mut out, CompressionType::Best, FilterType::Adaptive)
            .write_image(bytes, img.spec.width, img.spec.height, color)
            .map_err(|e| CodecError::Failed(e.to_string()))?;
        Ok(out)
    }
    fn decode(&self, enc: &[u8], _hint: &Spec) -> Result<Canonical, CodecError> {
        let img = image::load_from_memory_with_format(enc, ImageFormat::Png)
            .map_err(|e| CodecError::Failed(e.to_string()))?;
        Canonical::from_dynamic(img).map_err(CodecError::Failed)
    }
    /// Everything that is not IDAT payload: signature, IHDR, IEND, chunk framing.
    fn container_overhead(&self, enc: &[u8]) -> Option<u64> {
        let (mut pos, mut idat) = (8usize, 0u64);
        while pos + 12 <= enc.len() {
            let len = u32::from_be_bytes(enc[pos..pos + 4].try_into().ok()?) as usize;
            if &enc[pos + 4..pos + 8] == b"IDAT" {
                idat += len as u64;
            }
            pos += 12 + len;
        }
        Some(enc.len() as u64 - idat)
    }
}
