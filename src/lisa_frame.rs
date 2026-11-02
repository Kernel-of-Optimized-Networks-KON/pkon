//! Lisa's contribution to ULISA: the sensor frame of a 2x2 Bayer RAW mosaic.
//!
//! A frame is what Lisa knows about the sensor and nothing about how samples are predicted or coded:
//!   * frozen L1 lattice normalization (`pkon::Lattice`, unchanged): coded sample = `(original - origin) / g` at the lattice's coding depth;
//!   * the CFA description (validated by Lisa's own `ImageMetadata::validate`, which also checks the Bayer arrangement);
//!   * the four image-parity planes (plane index `(y & 1) * 2 + (x & 1)`, geometry from `lisa_bayer_plane::plane_geometry`);
//!   * the mosaic-flank topology: which already-coded parity planes sit on either side of a site in the mosaic.
//!
//! The topology depends on parity only, never on which colour a parity holds, so it is identical for RGGB/BGGR/GRBG/GBRG. Plane `k`
//! has `ox = k & 1`, `oy = k >> 1`. Its horizontal mosaic neighbours live in plane `k ^ 1` and its vertical ones in plane `k ^ 2`
//! (plane `k ^ 3` is only diagonal and is not used). Planes are coded in order 0, 1, 2, 3, so only neighbours with a smaller index are
//! linked. The two flank sites are `(x, x + 1)` when `ox == 1` and `(x - 1, x)` when `ox == 0` (likewise vertically); the sum of their
//! already-coded residuals is the flank sum that ELLIE's model conditions on.
//!
//! Anything the 2x2 Bayer implementation cannot represent is reported as [`FrameError::Unsupported`], never approximated.
use crate::ellie::{EllieError, FlankLink, FlankTopology};
use crate::lisa::{self, ImageMetadata, SampleLayout};
use crate::lisa_bayer_plane::plane_geometry;
use crate::pkon::Lattice;
use std::borrow::Cow;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// The input is valid but outside what the 2x2 Bayer frame covers (callers may fall back to the Lisa RAW path).
    Unsupported(&'static str),
    /// The input or the stored parameters are inconsistent.
    Invalid(String),
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::Unsupported(s) => write!(f, "unsupported by the Bayer frame: {s}"),
            FrameError::Invalid(s) => write!(f, "invalid Bayer frame: {s}"),
        }
    }
}
impl std::error::Error for FrameError {}

/// Whether `metadata` describes a mosaic the Bayer frame covers: Bayer layout, a valid 2x2 CFA, at least 2x2 samples.
pub fn check_supported(metadata: &ImageMetadata) -> Result<(), FrameError> {
    if metadata.layout != SampleLayout::BayerCfa {
        return Err(FrameError::Unsupported("sample layout is not a Bayer CFA"));
    }
    let Some(cfa) = &metadata.cfa else {
        return Err(FrameError::Unsupported("no CFA description"));
    };
    if (cfa.width, cfa.height) != (2, 2) {
        return Err(FrameError::Unsupported("CFA is not 2x2"));
    }
    if metadata.validate().is_err() {
        return Err(FrameError::Unsupported("CFA is not a valid RGB Bayer arrangement"));
    }
    if metadata.width < 2 || metadata.height < 2 {
        return Err(FrameError::Unsupported("mosaic is smaller than 2x2 (a parity plane would be empty)"));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BayerFrame {
    metadata: ImageMetadata,
    bit_depth: u8,
    lattice: Option<Lattice>,
    dims: [(usize, usize); 4],
}

impl BayerFrame {
    /// Frame of a whole mosaic: runs frozen L1 detection on the samples.
    pub fn build(samples: &[u16], bit_depth: u8, metadata: ImageMetadata) -> Result<Self, FrameError> {
        check_supported(&metadata)?;
        if !(1..=16).contains(&bit_depth) {
            return Err(FrameError::Invalid(format!("bit depth {bit_depth}")));
        }
        if samples.len() != metadata.width * metadata.height {
            return Err(FrameError::Invalid(format!("{} samples for a {}x{} mosaic", samples.len(), metadata.width, metadata.height)));
        }
        if samples.iter().any(|&v| u32::from(v) >> bit_depth != 0) {
            return Err(FrameError::Invalid("a sample exceeds the bit depth".into()));
        }
        let lattice = Lattice::detect(samples, bit_depth);
        Self::with_lattice(metadata, bit_depth, lattice)
    }

    /// Frame from stored parameters (the decoder side): the lattice is whatever the stream recorded.
    pub fn with_lattice(metadata: ImageMetadata, bit_depth: u8, lattice: Option<Lattice>) -> Result<Self, FrameError> {
        check_supported(&metadata)?;
        if !(1..=16).contains(&bit_depth) {
            return Err(FrameError::Invalid(format!("bit depth {bit_depth}")));
        }
        if let Some(l) = &lattice {
            l.validate(bit_depth).map_err(|e| FrameError::Invalid(e.to_string()))?;
        }
        let depth = lattice.map_or(bit_depth, |l| l.coding_bit_depth);
        lisa::validate(metadata.width, metadata.height, depth).map_err(|e| FrameError::Invalid(e.to_string()))?;
        let g = plane_geometry(metadata.width, metadata.height);
        let dims = [(g[0].2, g[0].3), (g[1].2, g[1].3), (g[2].2, g[2].3), (g[3].2, g[3].3)];
        if dims.iter().any(|&(w, h)| w == 0 || h == 0) {
            return Err(FrameError::Unsupported("a parity plane would be empty"));
        }
        Ok(Self { metadata, bit_depth, lattice, dims })
    }

    pub fn metadata(&self) -> &ImageMetadata {
        &self.metadata
    }
    pub fn bit_depth(&self) -> u8 {
        self.bit_depth
    }
    /// L1 lattice, `None` when the mosaic has no sample lattice (g = 1).
    pub fn lattice(&self) -> Option<Lattice> {
        self.lattice
    }
    /// Bit depth the planes are coded at: the lattice's coding depth, else the sample bit depth.
    pub fn coding_depth(&self) -> u8 {
        self.lattice.map_or(self.bit_depth, |l| l.coding_bit_depth)
    }
    /// `(width, height)` of the four parity planes.
    pub fn plane_dims(&self) -> [(usize, usize); 4] {
        self.dims
    }

    /// Mosaic samples as coded: L1-normalized when a lattice is active, otherwise the samples themselves.
    pub fn normalize<'a>(&self, samples: &'a [u16]) -> Cow<'a, [u16]> {
        match self.lattice {
            Some(l) => Cow::Owned(samples.iter().map(|&v| (u32::from(v - l.origin) / u32::from(l.g)) as u16).collect()),
            None => Cow::Borrowed(samples),
        }
    }

    /// Inverse of [`BayerFrame::normalize`], in place. Fails when a coded value would leave the sample range (corrupt stream).
    pub fn denormalize(&self, coded: &mut [u16]) -> Result<(), FrameError> {
        let max = (1u32 << self.bit_depth) - 1;
        if let Some(l) = self.lattice {
            for v in coded.iter_mut() {
                let o = u32::from(*v) * u32::from(l.g) + u32::from(l.origin);
                if o > max {
                    return Err(FrameError::Invalid("lattice reconstruction outside the sample range".into()));
                }
                *v = o as u16;
            }
        }
        Ok(())
    }

    /// De-interleave a coded mosaic into the four parity planes (row-major each).
    pub fn split(&self, coded: &[u16]) -> [Vec<u16>; 4] {
        let w = self.metadata.width;
        let mut planes: [Vec<u16>; 4] = std::array::from_fn(|k| vec![0u16; self.dims[k].0 * self.dims[k].1]);
        for (k, p) in planes.iter_mut().enumerate() {
            let (pw, ph) = self.dims[k];
            let (ox, oy) = (k & 1, k >> 1);
            for y in 0..ph {
                let row = (2 * y + oy) * w + ox;
                for x in 0..pw {
                    p[y * pw + x] = coded[row + 2 * x];
                }
            }
        }
        planes
    }

    /// Inverse of [`BayerFrame::split`].
    pub fn merge(&self, planes: &[Vec<u16>; 4]) -> Vec<u16> {
        let w = self.metadata.width;
        let mut out = vec![0u16; w * self.metadata.height];
        for (k, p) in planes.iter().enumerate() {
            let (pw, ph) = self.dims[k];
            let (ox, oy) = (k & 1, k >> 1);
            for y in 0..ph {
                let row = (2 * y + oy) * w + ox;
                for x in 0..pw {
                    out[row + 2 * x] = p[y * pw + x];
                }
            }
        }
        out
    }

    /// The mosaic-flank topology for planes coded in order 0, 1, 2, 3 (see the module comment).
    pub fn topology(&self) -> Result<FlankTopology, EllieError> {
        let links: [Vec<FlankLink>; 4] = std::array::from_fn(|k| {
            let (ox, oy) = (k & 1, k >> 1);
            let mut ls = Vec::new();
            for j in 0..k {
                if j == k ^ 1 {
                    let (a, b) = if ox == 1 { ((0, 0), (1, 0)) } else { ((-1, 0), (0, 0)) };
                    ls.push(FlankLink { plane: j as u8, a, b });
                } else if j == k ^ 2 {
                    let (a, b) = if oy == 1 { ((0, 0), (0, 1)) } else { ((0, -1), (0, 0)) };
                    ls.push(FlankLink { plane: j as u8, a, b });
                }
            }
            ls
        });
        FlankTopology::new(links)
    }
}
