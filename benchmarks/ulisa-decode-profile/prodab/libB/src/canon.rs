//! The canonical image representation every codec is measured on.
//!
//! Layout contract (the ONLY thing codecs are compared on):
//! - row-major, top row first, no row padding, channels interleaved
//! - 8-bit samples: one byte each; 16-bit samples: two bytes, little-endian
//! - channel order: Gray=[Y], GrayAlpha=[Y,A], Rgb=[R,G,B], Rgba=[R,G,B,A]
//! - alpha is straight (non-premultiplied); RGB under alpha=0 is real data and must survive
//!
//! Not part of the contract: ICC profile (carried and reported, not compared), EXIF, gamma,
//! animation, anything but the first frame.
use image::{DynamicImage, ImageDecoder, ImageReader};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Layout {
    Gray,
    GrayAlpha,
    Rgb,
    Rgba,
}

impl Layout {
    pub fn channels(self) -> usize {
        match self {
            Layout::Gray => 1,
            Layout::GrayAlpha => 2,
            Layout::Rgb => 3,
            Layout::Rgba => 4,
        }
    }
    pub fn channel_name(self, c: usize) -> &'static str {
        match (self, c) {
            (Layout::Gray, 0) => "Y",
            (Layout::GrayAlpha, 0) => "Y",
            (Layout::GrayAlpha, 1) => "A",
            (Layout::Rgb | Layout::Rgba, 0) => "R",
            (Layout::Rgb | Layout::Rgba, 1) => "G",
            (Layout::Rgb | Layout::Rgba, 2) => "B",
            (Layout::Rgba, 3) => "A",
            _ => "?",
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Layout::Gray => "gray",
            Layout::GrayAlpha => "graya",
            Layout::Rgb => "rgb",
            Layout::Rgba => "rgba",
        }
    }
    pub fn has_alpha(self) -> bool {
        matches!(self, Layout::GrayAlpha | Layout::Rgba)
    }
}

/// Everything about an image except its samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Spec {
    pub width: u32,
    pub height: u32,
    pub layout: Layout,
    pub bit_depth: u8,
}

impl Spec {
    pub fn bytes_per_sample(&self) -> usize {
        (self.bit_depth as usize) / 8
    }
    pub fn byte_len(&self) -> usize {
        self.width as usize * self.height as usize * self.layout.channels() * self.bytes_per_sample()
    }
    /// e.g. "rgba8", "gray16"
    pub fn label(&self) -> String {
        format!("{}{}", self.layout.name(), self.bit_depth)
    }
}

#[derive(Clone, Debug)]
pub struct Canonical {
    pub spec: Spec,
    pub data: Vec<u8>,
    /// Source ICC profile if the decoder exposed one. Reported, never compared.
    pub icc: Option<Vec<u8>>,
    /// Human-readable notes about anything the loader had to do (e.g. palette expansion).
    pub notes: Vec<String>,
}

impl Canonical {
    pub fn new(spec: Spec, data: Vec<u8>) -> Result<Self, String> {
        if spec.width == 0 || spec.height == 0 {
            return Err("zero-sized image".into());
        }
        if spec.bit_depth != 8 && spec.bit_depth != 16 {
            return Err(format!("unsupported bit depth {}", spec.bit_depth));
        }
        if data.len() != spec.byte_len() {
            return Err(format!("data length {} != expected {}", data.len(), spec.byte_len()));
        }
        Ok(Canonical { spec, data, icc: None, notes: vec![] })
    }

    pub fn sample(&self, x: u32, y: u32, c: usize) -> u16 {
        let i = ((y as usize * self.spec.width as usize + x as usize) * self.spec.layout.channels() + c)
            * self.spec.bytes_per_sample();
        if self.spec.bit_depth == 8 {
            self.data[i] as u16
        } else {
            u16::from_le_bytes([self.data[i], self.data[i + 1]])
        }
    }

    /// SHA-256 over a fixed header (dims, layout, depth) plus the samples.
    pub fn content_hash(&self) -> String {
        let mut h = Sha256::new();
        h.update(self.spec.width.to_le_bytes());
        h.update(self.spec.height.to_le_bytes());
        h.update([self.spec.layout.channels() as u8, self.spec.layout.has_alpha() as u8, self.spec.bit_depth]);
        h.update(&self.data);
        h.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn from_dynamic(img: DynamicImage) -> Result<Self, String> {
        fn le16(v: &[u16]) -> Vec<u8> {
            v.iter().flat_map(|s| s.to_le_bytes()).collect()
        }
        let (width, height) = (img.width(), img.height());
        let (layout, bit_depth, data) = match img {
            DynamicImage::ImageLuma8(b) => (Layout::Gray, 8, b.into_raw()),
            DynamicImage::ImageLumaA8(b) => (Layout::GrayAlpha, 8, b.into_raw()),
            DynamicImage::ImageRgb8(b) => (Layout::Rgb, 8, b.into_raw()),
            DynamicImage::ImageRgba8(b) => (Layout::Rgba, 8, b.into_raw()),
            DynamicImage::ImageLuma16(b) => (Layout::Gray, 16, le16(b.as_raw())),
            DynamicImage::ImageLumaA16(b) => (Layout::GrayAlpha, 16, le16(b.as_raw())),
            DynamicImage::ImageRgb16(b) => (Layout::Rgb, 16, le16(b.as_raw())),
            DynamicImage::ImageRgba16(b) => (Layout::Rgba, 16, le16(b.as_raw())),
            other => return Err(format!("unsupported pixel type {:?} (not 8/16-bit integer)", other.color())),
        };
        Canonical::new(Spec { width, height, layout, bit_depth }, data)
    }

    /// Load an image file into canonical form. Errors (never silently converts) on
    /// float/32-bit pixel types. Only PNG decoding is compiled in; add `image` features
    /// when external corpora need other formats.
    pub fn load(path: &Path) -> Result<Self, String> {
        Self::load_bytes(std::fs::read(path).map_err(|e| e.to_string())?)
    }

    /// Like `load`, from the complete bytes of an image file (format guessed from content).
    pub fn load_bytes(file_bytes: Vec<u8>) -> Result<Self, String> {
        let mut notes = vec![];
        // The decoder expands palette / <8-bit images; flag that, because it is a
        // representation change (exact for palette, scaled for low-bit gray).
        if file_bytes.len() > 26 && file_bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
            let (depth, ctype) = (file_bytes[24], file_bytes[25]);
            if ctype == 3 {
                notes.push(format!("PNG palette image ({depth}-bit) expanded to RGB(A) by decoder"));
            } else if depth < 8 {
                notes.push(format!("PNG {depth}-bit gray expanded to 8-bit by decoder (values scaled)"));
            }
        }
        let mut decoder = ImageReader::new(std::io::Cursor::new(file_bytes))
            .with_guessed_format()
            .map_err(|e| e.to_string())?
            .into_decoder()
            .map_err(|e| e.to_string())?;
        let icc = decoder.icc_profile().map_err(|e| e.to_string())?;
        let img = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
        let mut c = Canonical::from_dynamic(img)?;
        c.icc = icc;
        c.notes = notes;
        Ok(c)
    }
}

/// Where two canonical images first differ.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mismatch {
    Spec { expected: Spec, actual: Spec },
    Sample { x: u32, y: u32, channel: &'static str, expected: u16, actual: u16, differing_samples: u64 },
}

impl std::fmt::Display for Mismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Mismatch::Spec { expected, actual } => write!(f, "spec differs: expected {expected:?}, got {actual:?}"),
            Mismatch::Sample { x, y, channel, expected, actual, differing_samples } => write!(
                f,
                "first mismatch at (x={x}, y={y}) channel {channel}: expected {expected}, got {actual} ({differing_samples} differing samples total)"
            ),
        }
    }
}

/// Exact comparison of spec and every sample. `None` means bit-identical.
pub fn first_mismatch(expected: &Canonical, actual: &Canonical) -> Option<Mismatch> {
    if expected.spec != actual.spec {
        return Some(Mismatch::Spec { expected: expected.spec, actual: actual.spec });
    }
    let bps = expected.spec.bytes_per_sample();
    let mut first = None;
    let mut count = 0u64;
    for (i, (a, b)) in expected.data.chunks(bps).zip(actual.data.chunks(bps)).enumerate() {
        if a != b {
            count += 1;
            first.get_or_insert(i);
        }
    }
    let i = first?;
    let ch = expected.spec.layout.channels();
    let (pixel, c) = (i / ch, i % ch);
    let (x, y) = ((pixel % expected.spec.width as usize) as u32, (pixel / expected.spec.width as usize) as u32);
    Some(Mismatch::Sample {
        x,
        y,
        channel: expected.spec.layout.channel_name(c),
        expected: expected.sample(x, y, c),
        actual: actual.sample(x, y, c),
        differing_samples: count,
    })
}
