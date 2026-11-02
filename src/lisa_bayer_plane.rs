//! Bayer colour-plane Stage 1 (production default for `BayerCfa` metadata; the mosaic Stage 1 is the explicit fallback).
//!
//! The mosaic Stage 1 predicts on the interleaved mosaic, where W/N/NW are 1 pixel away and therefore other colours.
//! Here the four image-parity planes (the same planes Stage 2 builds) are each predicted on their own, with the unchanged
//! Stage 1 predictor (`lisa::encode` run on a plane), so every neighbour is a same-colour sample 2 pixels away. Plane
//! parity is taken in image coordinates, so all four CFA phases are covered without reading the pattern.
//!
//! The result is an ordinary [`Stage1Output`]: plane residuals are scattered back to the Morton slot of the mosaic pixel
//! they belong to. `organize_stage2` then lands each plane residual at its plane position (row-major), so Stage 2, 3
//! and 4 run unmodified. Non-Bayer layouts always use the mosaic Stage 1.
//!
//! The mode is recorded in the stream history: [`organize_stage2_with_mode`] writes `Stage1BayerPlanar` (planar) or the
//! legacy `Stage1Decorrelation` (mosaic) as the first record, and [`restore_stage1_with_mode`] reads it back, so the
//! decoder never needs out-of-band information. Streams made before this record existed carry `Stage1Decorrelation`
//! and decode as mosaic, unchanged. A planar record on non-Bayer metadata is rejected.
use crate::lisa::{self, ImageMetadata, LisaError, ReconstructionRecord, SampleLayout, Stage1Output, Stage2Output};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Stage1Mode {
    /// Fallback: Stage 1 on the interleaved mosaic (the pre-promotion production path).
    Mosaic,
    /// Production default: per-colour-plane Stage 1 for `BayerCfa` metadata (identical to `Mosaic` for every other layout).
    #[default]
    BayerPlanar,
}

impl Stage1Mode {
    /// The mode actually applied to `meta`: planar only for Bayer layouts.
    pub fn effective(self, meta: &ImageMetadata) -> Self {
        if is_planar(self, meta) { Stage1Mode::BayerPlanar } else { Stage1Mode::Mosaic }
    }
}

/// Stage 1 + Stage 2 with the mode recorded in the history. `Stage1Mode::default()` is the production choice.
pub fn organize_stage2_with_mode(stage1: Stage1Output, mode: Stage1Mode) -> Result<Stage2Output, LisaError> {
    let (planar, bit_depth) = (is_planar(mode, &stage1.metadata), stage1.bit_depth);
    let mut s2 = lisa::organize_stage2(stage1)?;
    if planar {
        s2.set_stage1_record(ReconstructionRecord::Stage1BayerPlanar { bit_depth });
    }
    Ok(s2)
}

/// Inverse of [`organize_stage2_with_mode`]: restores the Stage 1 residuals and the mode recorded in the stream.
pub fn restore_stage1_with_mode(out: &Stage2Output) -> Result<(Stage1Output, Stage1Mode), LisaError> {
    let (s1, inherited) = lisa::restore_stage1(out)?;
    let mode = match inherited.as_slice() {
        [ReconstructionRecord::Stage1Decorrelation { bit_depth }] if *bit_depth == s1.bit_depth => Stage1Mode::Mosaic,
        [ReconstructionRecord::Stage1BayerPlanar { bit_depth }] if *bit_depth == s1.bit_depth => {
            if s1.metadata.layout != SampleLayout::BayerCfa {
                return Err(LisaError::InvalidMetadata("planar Stage 1 record on non-Bayer metadata"));
            }
            Stage1Mode::BayerPlanar
        }
        _ => return Err(LisaError::InvalidMetadata("unrecognised Stage 1 record")),
    };
    Ok((s1, mode))
}

/// `(ox, oy, plane width, plane height)` for the four image-parity planes, index = `oy * 2 + ox`.
pub fn plane_geometry(width: usize, height: usize) -> [(usize, usize, usize, usize); 4] {
    let mut g = [(0, 0, 0, 0); 4];
    for (i, e) in g.iter_mut().enumerate() {
        let (ox, oy) = (i & 1, i >> 1);
        *e = (ox, oy, (width + 1 - ox) / 2, (height + 1 - oy) / 2);
    }
    g
}

/// Morton rank of every mosaic pixel, row-major.
fn morton_rank(width: usize, height: usize) -> Vec<u32> {
    let mut rank = vec![0u32; width * height];
    let mut k = 0u32;
    let _ = lisa::morton_visit::<()>(width, height, |x, y| {
        rank[y * width + x] = k;
        k += 1;
        Ok(())
    });
    rank
}

/// Plane residual sequence position -> plane (u, v), in the plane's own Morton order.
fn plane_order(pw: usize, ph: usize) -> Vec<(usize, usize)> {
    let mut v = Vec::with_capacity(pw * ph);
    let _ = lisa::morton_visit::<()>(pw, ph, |u, w| {
        v.push((u, w));
        Ok(())
    });
    v
}

fn is_planar(mode: Stage1Mode, meta: &ImageMetadata) -> bool {
    mode == Stage1Mode::BayerPlanar && meta.layout == SampleLayout::BayerCfa
}

/// Mode-aware counterpart of `lisa::encode_with_metadata`.
pub fn encode_stage1(
    samples: &[u16],
    width: usize,
    height: usize,
    bit_depth: u8,
    metadata: ImageMetadata,
    mode: Stage1Mode,
) -> Result<Stage1Output, LisaError> {
    if !is_planar(mode, &metadata) {
        return lisa::encode_with_metadata(samples, width, height, bit_depth, metadata);
    }
    metadata.validate()?;
    if (metadata.width, metadata.height) != (width, height) {
        return Err(LisaError::MetadataGeometryMismatch { expected: (width, height), actual: (metadata.width, metadata.height) });
    }
    let n = lisa::validate(width, height, bit_depth)?;
    if samples.len() != n {
        return Err(LisaError::LengthMismatch { expected: n, actual: samples.len() });
    }
    let rank = morton_rank(width, height);
    let mut residuals = vec![0u32; n];
    for (ox, oy, pw, ph) in plane_geometry(width, height) {
        if pw == 0 || ph == 0 {
            continue;
        }
        let plane: Vec<u16> = (0..ph).flat_map(|v| (0..pw).map(move |u| (u, v))).map(|(u, v)| samples[(oy + 2 * v) * width + ox + 2 * u]).collect();
        let res = lisa::encode(&plane, pw, ph, bit_depth)?;
        for (r, (u, v)) in res.into_iter().zip(plane_order(pw, ph)) {
            residuals[rank[(oy + 2 * v) * width + ox + 2 * u] as usize] = r;
        }
    }
    Ok(Stage1Output { residuals, metadata, bit_depth })
}

/// Main Stage 1 + Stage 2 entry point: Bayer layouts use the planar default, everything else the mosaic path; the mode
/// is recorded in the history. Use [`encode_stage2_with_mode`] to force the mosaic fallback.
pub fn encode_stage2(samples: &[u16], width: usize, height: usize, bit_depth: u8, metadata: ImageMetadata) -> Result<Stage2Output, LisaError> {
    encode_stage2_with_mode(samples, width, height, bit_depth, metadata, Stage1Mode::default())
}

/// [`encode_stage2`] with an explicit Stage 1 mode.
pub fn encode_stage2_with_mode(
    samples: &[u16],
    width: usize,
    height: usize,
    bit_depth: u8,
    metadata: ImageMetadata,
    mode: Stage1Mode,
) -> Result<Stage2Output, LisaError> {
    organize_stage2_with_mode(encode_stage1(samples, width, height, bit_depth, metadata, mode)?, mode)
}

/// Inverse of [`encode_stage2_with_mode`]: recovers the mode from the history and returns the row-major mosaic.
pub fn decode_stage2(out: &Stage2Output) -> Result<Vec<u16>, LisaError> {
    let (s1, mode) = restore_stage1_with_mode(out)?;
    decode_stage1(&s1, mode)
}

/// Mode-aware counterpart of `lisa::decode_with_metadata`: returns the row-major mosaic.
pub fn decode_stage1(out: &Stage1Output, mode: Stage1Mode) -> Result<Vec<u16>, LisaError> {
    if !is_planar(mode, &out.metadata) {
        return lisa::decode_with_metadata(out);
    }
    out.metadata.validate()?;
    let (width, height) = (out.metadata.width, out.metadata.height);
    let n = lisa::validate(width, height, out.bit_depth)?;
    if out.residuals.len() != n {
        return Err(LisaError::LengthMismatch { expected: n, actual: out.residuals.len() });
    }
    let rank = morton_rank(width, height);
    let mut samples = vec![0u16; n];
    for (ox, oy, pw, ph) in plane_geometry(width, height) {
        if pw == 0 || ph == 0 {
            continue;
        }
        let order = plane_order(pw, ph);
        let res: Vec<u32> = order.iter().map(|&(u, v)| out.residuals[rank[(oy + 2 * v) * width + ox + 2 * u] as usize]).collect();
        let plane = lisa::decode(&res, pw, ph, out.bit_depth)?;
        for v in 0..ph {
            for u in 0..pw {
                samples[(oy + 2 * v) * width + ox + 2 * u] = plane[v * pw + u];
            }
        }
    }
    Ok(samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planar_record_on_non_bayer_metadata_is_rejected() {
        let meta = ImageMetadata::without_cfa(4, 4, SampleLayout::Monochrome);
        let s1 = lisa::encode_with_metadata(&[7u16; 16], 4, 4, 10, meta).unwrap();
        let mut s2 = lisa::organize_stage2(s1).unwrap();
        s2.set_stage1_record(ReconstructionRecord::Stage1BayerPlanar { bit_depth: 10 });
        assert!(restore_stage1_with_mode(&s2).is_err());
    }

    #[test]
    fn stage1_record_bit_depth_must_agree() {
        let meta = ImageMetadata::without_cfa(4, 4, SampleLayout::Monochrome);
        let s1 = lisa::encode_with_metadata(&[7u16; 16], 4, 4, 10, meta).unwrap();
        let mut s2 = lisa::organize_stage2(s1).unwrap();
        s2.set_stage1_record(ReconstructionRecord::Stage1Decorrelation { bit_depth: 12 });
        assert!(restore_stage1_with_mode(&s2).is_err());
    }
}
