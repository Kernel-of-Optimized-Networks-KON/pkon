//! LISA Stage 1: structural decorrelation for RAW sample planes.
//!
//! Samples are visited in Morton (Z-order) order. Each sample is predicted from already reconstructed samples
//! (a neighbor is usable iff it is inside the image and already reconstructed), and the exact signed residual is
//! zigzag-mapped to an unsigned integer. Encoder and decoder run the identical `State`, whose decisions depend only on
//! reconstructed samples, so no side information is needed. No entropy coding happens here.
//!
//! Prediction per sample:
//!   1. Every pattern whose required neighbor offsets are all reconstructed is eligible (adaptive neighborhood).
//!   2. Each eligible pattern is scored by replaying it on nearby reconstructed samples (the training set) and summing
//!      a fixed-point log2 proxy of the zigzag residual (residual-entropy objective). Lowest cost wins; ties go to the
//!      lowest pattern index. If no pattern is scorable the first eligible one is used; if none is eligible the
//!      predictor is the mid-range value.
//!   3. A bounded per-(pattern, activity-context) bias estimate is added (locally estimated parameter) and the result
//!      is clamped to the sample range.

use crate::lisa_rp::{Est, Rp};
use crate::rangecoder::{BinDec, BinEnc, Bit, BitIo};
use std::fmt;

pub const MAX_DIM: usize = 1 << 20;
const MAX_SAMPLES: u64 = 1 << 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LisaError {
    ZeroDimension,
    DimensionTooLarge,
    UnsupportedBitDepth(u8),
    LengthMismatch { expected: usize, actual: usize },
    SampleOutOfRange { index: usize, value: u32 },
    CorruptResidual { index: usize },
    InvalidCfaDimensions { width: usize, height: usize },
    CfaPatternLengthMismatch { expected: usize, actual: usize },
    InvalidColorFilter(u8),
    CfaOriginOutOfRange { origin_x: usize, origin_y: usize },
    MetadataGeometryMismatch { expected: (usize, usize), actual: (usize, usize) },
    InvalidMetadata(&'static str),
    CorruptStream(&'static str),
}

impl fmt::Display for LisaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroDimension => write!(f, "width and height must be non-zero"),
            Self::DimensionTooLarge => write!(f, "dimensions exceed supported limits"),
            Self::UnsupportedBitDepth(b) => write!(f, "unsupported bit depth {b} (supported: 1..=16)"),
            Self::LengthMismatch { expected, actual } => write!(f, "expected {expected} values, got {actual}"),
            Self::SampleOutOfRange { index, value } => write!(f, "sample {index} = {value} exceeds bit depth"),
            Self::CorruptResidual { index } => write!(f, "residual {index} reconstructs outside the sample range"),
            Self::InvalidCfaDimensions { width, height } => write!(f, "invalid CFA repeat dimensions {width}x{height}"),
            Self::CfaPatternLengthMismatch { expected, actual } => {
                write!(f, "CFA pattern needs {expected} entries, got {actual}")
            }
            Self::InvalidColorFilter(c) => write!(f, "invalid color-filter identifier {c}"),
            Self::CfaOriginOutOfRange { origin_x, origin_y } => {
                write!(f, "CFA origin ({origin_x}, {origin_y}) lies outside the repeat pattern")
            }
            Self::MetadataGeometryMismatch { expected, actual } => write!(
                f,
                "metadata geometry {}x{} does not match image dimensions {}x{}",
                actual.0, actual.1, expected.0, expected.1
            ),
            Self::InvalidMetadata(why) => write!(f, "invalid image metadata: {why}"),
            Self::CorruptStream(why) => write!(f, "corrupt Stage 4 stream: {why}"),
        }
    }
}

impl std::error::Error for LisaError {}

/// Compatibility alias for the pre-rename error type name.
pub type PrawError = LisaError;

/// Validates dimensions and bit depth; returns the sample count.
pub fn validate(width: usize, height: usize, bit_depth: u8) -> Result<usize, LisaError> {
    if width == 0 || height == 0 {
        return Err(LisaError::ZeroDimension);
    }
    if width > MAX_DIM || height > MAX_DIM {
        return Err(LisaError::DimensionTooLarge);
    }
    if !(1..=16).contains(&bit_depth) {
        return Err(LisaError::UnsupportedBitDepth(bit_depth));
    }
    let n = (width as u64).checked_mul(height as u64).ok_or(LisaError::DimensionTooLarge)?;
    if n > MAX_SAMPLES {
        return Err(LisaError::DimensionTooLarge);
    }
    usize::try_from(n).map_err(|_| LisaError::DimensionTooLarge)
}

// ---------------------------------------------------------------- 1.1 Morton traversal

/// Calls `f(x, y)` for every pixel of a `width` x `height` image exactly once, in Morton order
/// (x bits at even positions, y bits at odd positions). Equivalent to enumerating the power-of-two square by Morton
/// index and skipping out-of-image coordinates, but quadrants entirely outside the image are pruned.
pub fn morton_visit<E>(width: usize, height: usize, mut f: impl FnMut(usize, usize) -> Result<(), E>) -> Result<(), E> {
    let side = width.max(height).next_power_of_two();
    visit(0, 0, side, width, height, &mut f)
}

fn visit<E>(
    x0: usize,
    y0: usize,
    size: usize,
    w: usize,
    h: usize,
    f: &mut impl FnMut(usize, usize) -> Result<(), E>,
) -> Result<(), E> {
    if x0 >= w || y0 >= h {
        return Ok(());
    }
    if size == 1 {
        return f(x0, y0);
    }
    let half = size / 2;
    visit(x0, y0, half, w, h, f)?;
    visit(x0 + half, y0, half, w, h, f)?;
    visit(x0, y0 + half, half, w, h, f)?;
    visit(x0 + half, y0 + half, half, w, h, f)
}

// ---------------------------------------------------------------- 1.5 Residual mapping

/// Signed -> unsigned zigzag: 0,-1,1,-2,2 -> 0,1,2,3,4. Bijective over all of i32.
#[inline]
pub fn zigzag(r: i32) -> u32 {
    let r = r as i64;
    (if r >= 0 { 2 * r } else { -2 * r - 1 }) as u32
}

/// Exact inverse of [`zigzag`].
#[inline]
pub fn unzigzag(u: u32) -> i32 {
    let u = u as i64;
    (if u & 1 == 0 { u / 2 } else { -(u + 1) / 2 }) as i32
}

// ---------------------------------------------------------------- 1.2 / 1.3 Patterns

type Off = (i32, i32);
const W: Off = (-1, 0);
const WW: Off = (-2, 0);
const N: Off = (0, -1);
const NN: Off = (0, -2);
const NW: Off = (-1, -1);
const NE: Off = (1, -1);
const NNE: Off = (1, -2);
const E: Off = (1, 0);
const S: Off = (0, 1);

/// Neighbour slots of a [`Window`]; the bit of slot `k` in `Window::mask` is `1 << k`.
const SLOTS: [Off; 9] = [W, WW, N, NN, NW, NE, NNE, E, S];
const M_W: u16 = 1;
const M_WW: u16 = 1 << 1;
const M_N: u16 = 1 << 2;
const M_NN: u16 = 1 << 3;
const M_NW: u16 = 1 << 4;
const M_NE: u16 = 1 << 5;
const M_NNE: u16 = 1 << 6;
const M_E: u16 = 1 << 7;
const M_S: u16 = 1 << 8;

const NPAT: usize = 14;

/// Required-neighbour masks of the candidate patterns, in fixed priority order (index = tie-break rank and fallback order).
const NEEDS: [u16; NPAT] = [
    M_W | M_N | M_NW,
    M_W,
    M_N,
    M_W | M_N | M_NW,
    M_W | M_N,
    M_W | M_NE,
    M_W | M_WW,
    M_N | M_NN,
    M_W | M_WW | M_N | M_NN | M_NW | M_NE | M_NNE,
    M_W | M_E,
    M_N | M_S,
    M_W | M_N | M_E | M_S,
    M_W | M_N | M_NE,
    M_NE,
];

fn med3(a: i32, b: i32, c: i32) -> i32 {
    a.max(b).min(a.min(b).max(c))
}

/// Reconstructed neighbours around one position, fetched once: `mask` marks available slots, `v` holds their values.
#[derive(Clone, Copy)]
struct Window {
    mask: u16,
    v: [i32; 9],
}

/// Unclamped prediction of pattern `p` from a window whose mask covers `NEEDS[p]`.
#[inline(always)]
fn eval(p: usize, v: &[i32; 9]) -> i32 {
    let (w, ww, n, nn, nw, ne, nne, e, s) = (v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7], v[8]);
    match p {
        0 => {
            if nw >= w.max(n) {
                w.min(n)
            } else if nw <= w.min(n) {
                w.max(n)
            } else {
                w + n - nw
            }
        }
        1 => w,
        2 => n,
        3 => w + n - nw,
        4 => (w + n + 1) >> 1,
        5 => (w + ne + 1) >> 1,
        6 => 2 * w - ww,
        7 => 2 * n - nn,
        8 => {
            let dh = ((w - ww).abs() + (n - nw).abs() + (n - ne).abs()) as i64;
            let dv = ((w - nw).abs() + (n - nn).abs() + (ne - nne).abs()) as i64;
            let s = dh + dv;
            if s == 0 {
                (w + n + 1) >> 1
            } else {
                ((w as i64 * dv + n as i64 * dh + s / 2) / s) as i32
            }
        }
        9 => (w + e + 1) >> 1,
        10 => (n + s + 1) >> 1,
        11 => (w + n + e + s + 2) >> 2,
        12 => med3(w, n, ne),
        _ => ne,
    }
}

/// Training positions (relative to the current sample) on which candidate patterns are replayed.
const TRAIN: &[Off] = &[W, N, NW, NE, WW, NN, E, S, (-1, 1)];

const CTX: usize = 8;
const BIAS_LIMIT: i32 = 32;

/// Fixed-point log2 proxy of v >= 1, 4 fractional bits (linear mantissa interpolation).
#[inline]
fn log2_q4(v: u32) -> u64 {
    let e = 31 - v.leading_zeros();
    let frac = if e >= 4 { (v >> (e - 4)) & 15 } else { (v << (4 - e)) & 15 };
    (e * 16 + frac) as u64
}

struct Prediction {
    value: i32,
    raw: i32,
    sel: Option<(usize, usize)>, // (pattern, context)
}

struct State {
    w: usize,
    h: usize,
    maxv: i32,
    rec: Vec<u16>,
    known: Vec<bool>,
    bias_sum: [[i32; CTX]; NPAT],
    bias_cnt: [[i32; CTX]; NPAT],
    shift: u32,
}

impl State {
    fn new(w: usize, h: usize, bit_depth: u8, n: usize) -> Self {
        State {
            w,
            h,
            maxv: (1i32 << bit_depth) - 1,
            rec: vec![0; n],
            known: vec![false; n],
            bias_sum: [[0; CTX]; NPAT],
            bias_cnt: [[0; CTX]; NPAT],
            shift: (bit_depth as u32).saturating_sub(8),
        }
    }

    /// All nine neighbour slots of (x, y), each fetched once (None-equivalent: bit clear in `mask`).
    #[inline]
    fn window(&self, x: usize, y: usize) -> Window {
        let mut win = Window { mask: 0, v: [0; 9] };
        for (k, &(dx, dy)) in SLOTS.iter().enumerate() {
            let nx = x as i64 + dx as i64;
            let ny = y as i64 + dy as i64;
            if nx < 0 || ny < 0 || nx >= self.w as i64 || ny >= self.h as i64 {
                continue;
            }
            let i = ny as usize * self.w + nx as usize;
            if self.known[i] {
                win.mask |= 1 << k;
                win.v[k] = self.rec[i] as i32;
            }
        }
        win
    }

    /// Reconstructed value at (x+dx, y+dy), or None if outside the image or not yet reconstructed.
    #[inline]
    fn fetch(&self, x: usize, y: usize, (dx, dy): Off) -> Option<i32> {
        let nx = x as i64 + dx as i64;
        let ny = y as i64 + dy as i64;
        if nx < 0 || ny < 0 || nx >= self.w as i64 || ny >= self.h as i64 {
            return None;
        }
        let i = ny as usize * self.w + nx as usize;
        self.known[i].then(|| self.rec[i] as i32)
    }

    /// Clamped prediction of pattern `p` from `win`; None if any required neighbor is unavailable.
    #[inline(always)]
    fn predict_win(&self, p: usize, win: &Window) -> Option<i32> {
        (win.mask & NEEDS[p] == NEEDS[p]).then(|| eval(p, &win.v).clamp(0, self.maxv))
    }

    /// Local activity context from W, N, NW, NE (0 if any is unavailable).
    fn context(&self, win: &Window) -> usize {
        const NEED: u16 = M_W | M_N | M_NW | M_NE;
        if win.mask & NEED != NEED {
            return 0;
        }
        let (w, n, nw, ne) = (win.v[0], win.v[2], win.v[4], win.v[5]);
        let a = ((w - nw).abs() + (n - nw).abs() + (n - ne).abs()) as u32 >> self.shift;
        ((32 - a.leading_zeros()) as usize).min(CTX - 1)
    }

    fn predict(&self, x: usize, y: usize) -> Prediction {
        let cw = self.window(x, y);
        // Training samples: actual values and the windows around them, resolved once for all candidates.
        let mut train: [(i32, Window); 9] = [(0, Window { mask: 0, v: [0; 9] }); 9];
        let mut nt = 0;
        for &(dx, dy) in TRAIN {
            let Some(actual) = self.fetch(x, y, (dx, dy)) else { continue };
            let (qx, qy) = ((x as i64 + dx as i64) as usize, (y as i64 + dy as i64) as usize);
            train[nt] = (actual, self.window(qx, qy));
            nt += 1;
        }
        let mut best: Option<(u64, usize)> = None; // scorable winner
        let mut first_eligible: Option<usize> = None;
        for p in 0..NPAT {
            if cw.mask & NEEDS[p] != NEEDS[p] {
                continue;
            }
            first_eligible.get_or_insert(p);
            let mut cost = 0u64;
            let mut terms = 0u32;
            for (actual, qw) in &train[..nt] {
                let Some(pred) = self.predict_win(p, qw) else { continue };
                cost += log2_q4(zigzag(actual - pred) + 1);
                terms += 1;
            }
            if terms > 0 && best.map_or(true, |(bc, _)| cost < bc) {
                best = Some((cost, p));
            }
        }
        let Some(p) = best.map(|b| b.1).or(first_eligible) else {
            let mid = 1 << (self.maxv.count_ones() - 1);
            return Prediction { value: mid, raw: mid, sel: None };
        };
        let raw = self.predict_win(p, &cw).unwrap_or(0);
        let ctx = self.context(&cw);
        let (s, c) = (self.bias_sum[p][ctx], self.bias_cnt[p][ctx]);
        let corr = if c > 0 { (s + (c >> 1)).div_euclid(c) } else { 0 };
        Prediction { value: (raw + corr).clamp(0, self.maxv), raw, sel: Some((p, ctx)) }
    }

    fn update(&mut self, x: usize, y: usize, value: i32, pred: &Prediction) {
        let i = y * self.w + x;
        self.rec[i] = value as u16;
        self.known[i] = true;
        if let Some((p, ctx)) = pred.sel {
            self.bias_sum[p][ctx] += value - pred.raw;
            self.bias_cnt[p][ctx] += 1;
            if self.bias_cnt[p][ctx] >= BIAS_LIMIT {
                self.bias_sum[p][ctx] >>= 1;
                self.bias_cnt[p][ctx] >>= 1;
            }
        }
    }
}

// ---------------------------------------------------------------- 1.7 Encode / decode

/// Encodes a row-major `width` x `height` sample plane. Returns the zigzag-mapped residuals in Morton order.
pub fn encode(samples: &[u16], width: usize, height: usize, bit_depth: u8) -> Result<Vec<u32>, LisaError> {
    let n = validate(width, height, bit_depth)?;
    if samples.len() != n {
        return Err(LisaError::LengthMismatch { expected: n, actual: samples.len() });
    }
    let maxv = (1u32 << bit_depth) - 1;
    if let Some((index, &v)) = samples.iter().enumerate().find(|(_, &v)| v as u32 > maxv) {
        return Err(LisaError::SampleOutOfRange { index, value: v as u32 });
    }
    let mut st = State::new(width, height, bit_depth, n);
    let mut out = Vec::with_capacity(n);
    morton_visit(width, height, |x, y| {
        let pred = st.predict(x, y);
        let v = samples[y * width + x] as i32;
        out.push(zigzag(v - pred.value));
        st.update(x, y, v, &pred);
        Ok(())
    })?;
    Ok(out)
}

/// Inverse of [`encode`]: returns the row-major sample plane.
pub fn decode(residuals: &[u32], width: usize, height: usize, bit_depth: u8) -> Result<Vec<u16>, LisaError> {
    let n = validate(width, height, bit_depth)?;
    if residuals.len() != n {
        return Err(LisaError::LengthMismatch { expected: n, actual: residuals.len() });
    }
    let mut st = State::new(width, height, bit_depth, n);
    let mut out = vec![0u16; n];
    let mut k = 0usize;
    morton_visit(width, height, |x, y| {
        let pred = st.predict(x, y);
        let v = pred.value as i64 + unzigzag(residuals[k]) as i64;
        if v < 0 || v > st.maxv as i64 {
            return Err(LisaError::CorruptResidual { index: k });
        }
        st.update(x, y, v as i32, &pred);
        out[y * width + x] = v as u16;
        k += 1;
        Ok(())
    })?;
    Ok(out)
}

// ---------------------------------------------------------------- 1.8 Image metadata handoff (descriptive only)
//
// Metadata describes the ORIGINAL samples. It is never an input to prediction, traversal, or residual mapping; the
// metadata-aware API below only validates it and carries it alongside the unchanged Stage 1 residual stream.

/// How the original samples are organized. Non-exhaustive so further layouts can be added without breaking callers.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleLayout {
    Unspecified,
    Monochrome,
    /// Linear / multichannel data (no CFA).
    Linear,
    /// Bayer mosaic: a 2x2 pattern with one red, one blue and two diagonal greens (any arrangement).
    BayerCfa,
    /// Any other CFA mosaic with an arbitrary repeat size.
    OtherCfa,
}

/// Color-filter identity of one CFA site. Discriminants follow the DNG `CFAPattern` codes.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ColorFilter {
    Red = 0,
    Green = 1,
    Blue = 2,
    Cyan = 3,
    Magenta = 4,
    Yellow = 5,
    White = 6,
}

impl TryFrom<u8> for ColorFilter {
    type Error = LisaError;

    fn try_from(code: u8) -> Result<Self, LisaError> {
        Ok(match code {
            0 => Self::Red,
            1 => Self::Green,
            2 => Self::Blue,
            3 => Self::Cyan,
            4 => Self::Magenta,
            5 => Self::Yellow,
            6 => Self::White,
            c => return Err(LisaError::InvalidColorFilter(c)),
        })
    }
}

/// A repeating CFA pattern. `entries` is row-major (`width` x `height`). The image sample at (0, 0) corresponds to
/// pattern position (`origin_x`, `origin_y`); the pattern is stored exactly as supplied, never normalized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CfaPattern {
    pub width: usize,
    pub height: usize,
    pub entries: Vec<ColorFilter>,
    pub origin_x: usize,
    pub origin_y: usize,
}

impl CfaPattern {
    /// Color filter at image coordinate (x, y). The pattern must have passed validation.
    pub fn color_at(&self, x: usize, y: usize) -> ColorFilter {
        self.entries[((y + self.origin_y) % self.height) * self.width + (x + self.origin_x) % self.width]
    }

    fn validate(&self) -> Result<(), LisaError> {
        let bad_dims = LisaError::InvalidCfaDimensions { width: self.width, height: self.height };
        if self.width == 0 || self.height == 0 || self.width > MAX_DIM || self.height > MAX_DIM {
            return Err(bad_dims);
        }
        let expected = self.width.checked_mul(self.height).ok_or(bad_dims)?;
        if self.entries.len() != expected {
            return Err(LisaError::CfaPatternLengthMismatch { expected, actual: self.entries.len() });
        }
        if self.origin_x >= self.width || self.origin_y >= self.height {
            return Err(LisaError::CfaOriginOutOfRange { origin_x: self.origin_x, origin_y: self.origin_y });
        }
        Ok(())
    }

    fn validate_bayer(&self) -> Result<(), LisaError> {
        if (self.width, self.height) != (2, 2) {
            return Err(LisaError::InvalidMetadata("Bayer CFA requires a 2x2 repeat"));
        }
        let count = |c| self.entries.iter().filter(|&&e| e == c).count();
        if count(ColorFilter::Red) != 1 || count(ColorFilter::Blue) != 1 || count(ColorFilter::Green) != 2 {
            return Err(LisaError::InvalidMetadata("Bayer CFA requires one red, one blue and two green entries"));
        }
        let g = ColorFilter::Green;
        let diagonal = (self.entries[0] == g && self.entries[3] == g) || (self.entries[1] == g && self.entries[2] == g);
        if !diagonal {
            return Err(LisaError::InvalidMetadata("Bayer CFA greens must lie on a diagonal"));
        }
        Ok(())
    }
}

/// Descriptive information about the original image samples, supplied by the upstream input layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageMetadata {
    pub width: usize,
    pub height: usize,
    pub layout: SampleLayout,
    /// Required for `BayerCfa` / `OtherCfa`; must be `None` for all other layouts.
    pub cfa: Option<CfaPattern>,
}

impl ImageMetadata {
    /// Metadata for a non-CFA layout (`Unspecified`, `Monochrome`, `Linear`).
    pub fn without_cfa(width: usize, height: usize, layout: SampleLayout) -> Self {
        ImageMetadata { width, height, layout, cfa: None }
    }

    /// Metadata for a CFA layout; the pattern and phase are taken exactly as given.
    pub fn with_cfa(width: usize, height: usize, layout: SampleLayout, cfa: CfaPattern) -> Self {
        ImageMetadata { width, height, layout, cfa: Some(cfa) }
    }

    /// Deterministic structural validation. Does not check bit depth or sample data.
    pub fn validate(&self) -> Result<(), LisaError> {
        if self.width == 0 || self.height == 0 {
            return Err(LisaError::ZeroDimension);
        }
        if self.width > MAX_DIM || self.height > MAX_DIM {
            return Err(LisaError::DimensionTooLarge);
        }
        match (self.layout, &self.cfa) {
            (SampleLayout::BayerCfa, Some(p)) => {
                p.validate()?;
                p.validate_bayer()
            }
            (SampleLayout::OtherCfa, Some(p)) => p.validate(),
            (SampleLayout::BayerCfa | SampleLayout::OtherCfa, None) => {
                Err(LisaError::InvalidMetadata("CFA layout requires a CFA pattern"))
            }
            (SampleLayout::Unspecified | SampleLayout::Monochrome | SampleLayout::Linear, None) => Ok(()),
            (SampleLayout::Unspecified | SampleLayout::Monochrome | SampleLayout::Linear, Some(_)) => {
                Err(LisaError::InvalidMetadata("CFA pattern supplied for a non-CFA layout"))
            }
        }
    }
}

/// Stage 1 output: the unchanged Morton-order residual stream plus the separate descriptive metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stage1Output {
    /// Zigzag-mapped residuals, identical to [`encode`] output for the same samples and dimensions.
    pub residuals: Vec<u32>,
    pub metadata: ImageMetadata,
    pub bit_depth: u8,
}

impl Stage1Output {
    /// Residual `k` belongs to the pixel visited `k`-th by [`morton_visit`] over `metadata.width` x `metadata.height`
    /// (x bits at even Morton positions, y bits at odd positions, out-of-image coordinates skipped).
    pub fn coordinates(&self) -> Vec<(usize, usize)> {
        let mut v = Vec::with_capacity(self.residuals.len());
        let _ = morton_visit::<()>(self.metadata.width, self.metadata.height, |x, y| {
            v.push((x, y));
            Ok(())
        });
        v
    }
}

/// Metadata-aware [`encode`] (mosaic Stage 1 only; this is the explicit fallback path). The production entry point is
/// `lisa_bayer_plane::encode_stage2`, which defaults to per-plane Stage 1 for Bayer layouts. `metadata` must be valid and match `width` x `height`; it does not affect the residuals.
pub fn encode_with_metadata(
    samples: &[u16],
    width: usize,
    height: usize,
    bit_depth: u8,
    metadata: ImageMetadata,
) -> Result<Stage1Output, LisaError> {
    metadata.validate()?;
    if (metadata.width, metadata.height) != (width, height) {
        return Err(LisaError::MetadataGeometryMismatch {
            expected: (width, height),
            actual: (metadata.width, metadata.height),
        });
    }
    let residuals = encode(samples, width, height, bit_depth)?;
    Ok(Stage1Output { residuals, metadata, bit_depth })
}

/// Metadata-aware [`decode`]: returns the row-major sample plane for `out.metadata` dimensions.
pub fn decode_with_metadata(out: &Stage1Output) -> Result<Vec<u16>, LisaError> {
    out.metadata.validate()?;
    decode(&out.residuals, out.metadata.width, out.metadata.height, out.bit_depth)
}

// ---------------------------------------------------------------- Stage 2: Bayer-aware organization
//
// Stage 2 is a reversible permutation of the Stage 1 residuals. Values are never altered. The output carries the data
// together with the cumulative reconstruction history (oldest record first); reversal pops records newest-first.

/// One applied transformation, with everything needed to reverse it. Later stages append their own variants.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconstructionRecord {
    /// Stage 1 structural decorrelation (Morton-order zigzag residuals) at the given bit depth.
    Stage1Decorrelation { bit_depth: u8 },
    /// Stage 1 per-colour-plane decorrelation (`lisa_bayer_plane`, `BayerCfa` metadata only). Older streams never carry this
    /// record, so they keep decoding as the mosaic path above.
    Stage1BayerPlanar { bit_depth: u8 },
    /// Stage 2 organization of the Stage 1 residuals.
    Stage2Organization(Stage2Record),
    /// Stage 3 context classification. Residual values are unchanged; the record only identifies the fixed rule set.
    Stage3Context(Stage3Record),
    /// Stage 4 entropy coding. Residuals are recoverable only by decoding the payload under this record's rules.
    Stage4Entropy(Stage4Record),
}

/// Identity of a Bayer plane. `Green1` is the green sharing a CFA row with red; `Green2` shares a row with blue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaneRole {
    Red,
    Green1,
    Green2,
    Blue,
}

/// Geometry of one Bayer plane. Plane `i` holds the pixels (`offset_x + 2*u`, `offset_y + 2*v`) in row-major (u, v)
/// order, with `i = offset_y * 2 + offset_x` in image coordinates (not pattern coordinates).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaneInfo {
    pub role: PlaneRole,
    pub color: ColorFilter,
    pub offset_x: usize,
    pub offset_y: usize,
    pub width: usize,
    pub height: usize,
}

/// How Stage 2 organized the residuals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage2Mode {
    /// Residuals kept in Stage 1 Morton order.
    Identity,
    /// Four CFA-position planes, in image-parity order (0,0), (1,0), (0,1), (1,1).
    BayerPlanes { cfa: CfaPattern, planes: [PlaneInfo; 4] },
}

/// Stage 2 reconstruction record: the image geometry and the organization applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stage2Record {
    pub width: usize,
    pub height: usize,
    pub mode: Stage2Mode,
}

/// Organized residual data. The variant must agree with the newest [`Stage2Record`] in the history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrganizedResiduals {
    /// Stage 1 residual sequence, Morton order, unchanged.
    Morton(Vec<u32>),
    /// One residual vector per plane, ordered as in [`Stage2Mode::BayerPlanes`].
    BayerPlanes([Vec<u32>; 4]),
}

/// Stage 2 output: organized residuals bound to the metadata and reconstruction history that describe them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stage2Output {
    data: OrganizedResiduals,
    metadata: ImageMetadata,
    bit_depth: u8,
    history: Vec<ReconstructionRecord>,
}

impl Stage2Output {
    /// Replaces the Stage 1 record (history[0]); used by `lisa_bayer_plane` to record its mode.
    pub(crate) fn set_stage1_record(&mut self, record: ReconstructionRecord) {
        self.history[0] = record;
    }

    pub fn data(&self) -> &OrganizedResiduals {
        &self.data
    }
    pub fn metadata(&self) -> &ImageMetadata {
        &self.metadata
    }
    pub fn bit_depth(&self) -> u8 {
        self.bit_depth
    }
    /// Cumulative history, oldest first; reverse in the opposite order.
    pub fn history(&self) -> &[ReconstructionRecord] {
        &self.history
    }
    /// Organization mode of the Stage 2 record, if it is the newest one.
    pub fn mode(&self) -> Option<&Stage2Mode> {
        match self.history.last() {
            Some(ReconstructionRecord::Stage2Organization(r)) => Some(&r.mode),
            _ => None,
        }
    }
    /// Appends a record for a later stage's transformation together with the data it produced, in a single step so
    /// data and history cannot be mismatched.
    pub fn extend(&mut self, record: ReconstructionRecord, data: OrganizedResiduals) {
        self.history.push(record);
        self.data = data;
    }
}

fn plane_dims(width: usize, height: usize, ox: usize, oy: usize) -> Result<(usize, usize), LisaError> {
    let dim = |n: usize, o: usize| n.checked_add(1).and_then(|v| v.checked_sub(o)).map(|v| v / 2);
    match (dim(width, ox), dim(height, oy)) {
        (Some(w), Some(h)) => Ok((w, h)),
        _ => Err(LisaError::DimensionTooLarge),
    }
}

fn bayer_planes(width: usize, height: usize, cfa: &CfaPattern) -> Result<[PlaneInfo; 4], LisaError> {
    let color = |i: usize| cfa.color_at(i & 1, i >> 1);
    let red_i = (0..4)
        .find(|&i| color(i) == ColorFilter::Red)
        .ok_or(LisaError::InvalidMetadata("Bayer CFA has no red"))?;
    let mut planes = [PlaneInfo {
        role: PlaneRole::Red,
        color: ColorFilter::Red,
        offset_x: 0,
        offset_y: 0,
        width: 0,
        height: 0,
    }; 4];
    for (i, p) in planes.iter_mut().enumerate() {
        let (ox, oy) = (i & 1, i >> 1);
        let c = color(i);
        let role = match c {
            ColorFilter::Red => PlaneRole::Red,
            ColorFilter::Blue => PlaneRole::Blue,
            ColorFilter::Green if oy == red_i >> 1 => PlaneRole::Green1,
            ColorFilter::Green => PlaneRole::Green2,
            _ => return Err(LisaError::InvalidMetadata("Bayer CFA contains a non-RGB entry")),
        };
        let (w, h) = plane_dims(width, height, ox, oy)?;
        *p = PlaneInfo { role, color: c, offset_x: ox, offset_y: oy, width: w, height: h };
    }
    Ok(planes)
}

/// Index of the pixel (x, y) within its plane's vector, with checked arithmetic.
fn plane_index(planes: &[PlaneInfo; 4], x: usize, y: usize) -> Result<(usize, usize), LisaError> {
    let pi = (y & 1) * 2 + (x & 1);
    let i = (y >> 1)
        .checked_mul(planes[pi].width)
        .and_then(|v| v.checked_add(x >> 1))
        .ok_or(LisaError::InvalidMetadata("impossible Bayer plane mapping"))?;
    Ok((pi, i))
}

/// Stage 2 entry point: organizes Stage 1's residuals. Bayer mosaics are de-interleaved into four CFA-position planes;
/// every other supported layout passes through unchanged. Consumes the Stage 1 result so the data cannot be detached
/// from its metadata.
pub fn organize_stage2(stage1: Stage1Output) -> Result<Stage2Output, LisaError> {
    let Stage1Output { residuals, metadata, bit_depth } = stage1;
    metadata.validate()?;
    let (width, height) = (metadata.width, metadata.height);
    let n = validate(width, height, bit_depth)?;
    if residuals.len() != n {
        return Err(LisaError::LengthMismatch { expected: n, actual: residuals.len() });
    }

    let (mode, data) = match (metadata.layout, &metadata.cfa) {
        (SampleLayout::BayerCfa, Some(cfa)) => {
            let planes = bayer_planes(width, height, cfa)?;
            let mut total = 0usize;
            let mut out: [Vec<u32>; 4] = Default::default();
            for (v, p) in out.iter_mut().zip(&planes) {
                let len = p.width.checked_mul(p.height).ok_or(LisaError::DimensionTooLarge)?;
                total = total.checked_add(len).ok_or(LisaError::DimensionTooLarge)?;
                *v = vec![0; len];
            }
            if total != n {
                return Err(LisaError::InvalidMetadata("Bayer plane sizes do not cover the image"));
            }
            let mut k = 0usize;
            morton_visit::<LisaError>(width, height, |x, y| {
                let (pi, i) = plane_index(&planes, x, y)?;
                *out[pi].get_mut(i).ok_or(LisaError::InvalidMetadata("impossible Bayer plane mapping"))? = residuals[k];
                k += 1;
                Ok(())
            })?;
            (Stage2Mode::BayerPlanes { cfa: cfa.clone(), planes }, OrganizedResiduals::BayerPlanes(out))
        }
        (SampleLayout::BayerCfa, None) => {
            return Err(LisaError::InvalidMetadata("CFA layout requires a CFA pattern"));
        }
        (SampleLayout::Unspecified | SampleLayout::Monochrome | SampleLayout::Linear | SampleLayout::OtherCfa, _) => {
            (Stage2Mode::Identity, OrganizedResiduals::Morton(residuals))
        }
    };

    let history = vec![
        ReconstructionRecord::Stage1Decorrelation { bit_depth },
        ReconstructionRecord::Stage2Organization(Stage2Record { width, height, mode }),
    ];
    Ok(Stage2Output { data, metadata, bit_depth, history })
}

/// Exact inverse of [`organize_stage2`]: restores the Stage 1 residual sequence (Morton order, values unchanged) and
/// returns it with the inherited history that precedes the Stage 2 record. Requires Stage 2 to be the newest record.
pub fn restore_stage1(out: &Stage2Output) -> Result<(Stage1Output, Vec<ReconstructionRecord>), LisaError> {
    let Some((ReconstructionRecord::Stage2Organization(rec), inherited)) = out.history.split_last() else {
        return Err(LisaError::InvalidMetadata("newest reconstruction record is not Stage 2"));
    };
    out.metadata.validate()?;
    let (width, height) = (out.metadata.width, out.metadata.height);
    if (rec.width, rec.height) != (width, height) {
        return Err(LisaError::MetadataGeometryMismatch { expected: (width, height), actual: (rec.width, rec.height) });
    }
    let n = validate(width, height, out.bit_depth)?;

    let residuals = match (&rec.mode, &out.data) {
        (Stage2Mode::Identity, OrganizedResiduals::Morton(r)) => {
            if r.len() != n {
                return Err(LisaError::LengthMismatch { expected: n, actual: r.len() });
            }
            r.clone()
        }
        (Stage2Mode::BayerPlanes { cfa, planes }, OrganizedResiduals::BayerPlanes(data)) => {
            if out.metadata.cfa.as_ref() != Some(cfa) || bayer_planes(width, height, cfa)? != *planes {
                return Err(LisaError::InvalidMetadata("Stage 2 record disagrees with metadata"));
            }
            for (v, p) in data.iter().zip(planes) {
                let len = p.width.checked_mul(p.height).ok_or(LisaError::DimensionTooLarge)?;
                if v.len() != len {
                    return Err(LisaError::LengthMismatch { expected: len, actual: v.len() });
                }
            }
            let mut r = Vec::with_capacity(n);
            morton_visit::<LisaError>(width, height, |x, y| {
                let (pi, i) = plane_index(planes, x, y)?;
                r.push(*data[pi].get(i).ok_or(LisaError::InvalidMetadata("impossible Bayer plane mapping"))?);
                Ok(())
            })?;
            r
        }
        _ => return Err(LisaError::InvalidMetadata("organized data does not match the Stage 2 record")),
    };
    Ok((Stage1Output { residuals, metadata: out.metadata.clone(), bit_depth: out.bit_depth }, inherited.to_vec()))
}

// ---------------------------------------------------------------- Stage 3: deterministic context classification
//
// Stage 3 leaves every residual unchanged and labels each one with a local-activity class derived only from residuals
// that precede it in the coding order (left, above, upper-left). A decoder reproduces each label from already decoded
// residuals before decoding the residual itself, so no label stream is stored. No probabilities or coding live here.
//
// Activity score = mean of the eligible neighbors' zigzag residuals, rounded half up in integer arithmetic:
//   score = (sum + n/2) / n, with n = number of eligible neighbors (1..=3). No eligible neighbor => Low.
// Classes (fixed thresholds, upper bounds exclusive): Low < 3 <= Moderate < 12 <= High < 48 <= VeryHigh.

/// Version of the production Stage 3 rule set: 2 = OR-128 (128 contexts, `lisa_or128`). Bump if the rule ever changes.
pub const STAGE3_RULE_VERSION: u8 = 2;
/// Legacy Stage 3 rule set (version 1): the four-class activity rule below. Still decodable, no longer produced by
/// default; select it with [`classify_stage3_with`].
pub const STAGE3_LEGACY_RULE_VERSION: u8 = 1;
/// OR-128 learned context tree (rule version 3, `lisa_or128_tree`). Selectable with [`classify_stage3_with`]; not the default.
pub const STAGE3_OR128_TREE_RULE_VERSION: u8 = 3;
/// Reduced OR-128 trees (opt-in experiments): the first 63 / 31 splits of the 128-leaf tree.
pub const STAGE3_OR128_TREE64_RULE_VERSION: u8 = 4;
pub const STAGE3_OR128_TREE32_RULE_VERSION: u8 = 5;
/// First score of Moderate, High and VeryHigh respectively (legacy rule).
pub const STAGE3_THRESHOLDS: [u32; 3] = [3, 12, 48];

/// Stage 3 context rule. Identified in the stream by [`Stage3Record::rule_version`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Stage3Rule {
    /// Version 1: four activity classes from the rounded mean of the causal neighbors.
    Legacy4,
    /// Version 2 (production): OR-128, 128 contexts from the frozen table in `lisa_or128`.
    #[default]
    Or128,
    /// Version 3 (opt-in): OR-128 with the fixed table replaced by the learned decision tree in `lisa_or128_tree`.
    Or128Tree,
    /// Version 4 (opt-in experiment): 64-leaf truncation of the learned tree.
    Or128Tree64,
    /// Version 5 (opt-in experiment): 32-leaf truncation of the learned tree.
    Or128Tree32,
}

impl Stage3Rule {
    pub fn version(self) -> u8 {
        match self {
            Stage3Rule::Legacy4 => STAGE3_LEGACY_RULE_VERSION,
            Stage3Rule::Or128 => STAGE3_RULE_VERSION,
            Stage3Rule::Or128Tree => STAGE3_OR128_TREE_RULE_VERSION,
            Stage3Rule::Or128Tree64 => STAGE3_OR128_TREE64_RULE_VERSION,
            Stage3Rule::Or128Tree32 => STAGE3_OR128_TREE32_RULE_VERSION,
        }
    }
    /// Number of contexts (and Stage 4 models) the rule produces.
    pub fn context_count(self) -> usize {
        match self {
            Stage3Rule::Legacy4 => ActivityClass::COUNT,
            Stage3Rule::Or128 => crate::lisa_or128::OR128_K,
            Stage3Rule::Or128Tree => crate::lisa_or128_tree::OR128_TREE_LEAVES,
            Stage3Rule::Or128Tree64 => crate::lisa_or128_tree_small_data::l64::OR128_TREE_LEAVES,
            Stage3Rule::Or128Tree32 => crate::lisa_or128_tree_small_data::l32::OR128_TREE_LEAVES,
        }
    }
    /// Context id of one residual from its causal neighbors; always `< context_count()`.
    #[inline]
    pub fn context(self, left: Option<u32>, above: Option<u32>, above_left: Option<u32>) -> u8 {
        match self {
            Stage3Rule::Legacy4 => classify_activity(left, above, above_left) as u8,
            Stage3Rule::Or128 => crate::lisa_or128::context(left, above, above_left),
            Stage3Rule::Or128Tree => crate::lisa_or128_tree::context(left, above, above_left),
            Stage3Rule::Or128Tree64 => crate::lisa_or128_tree::context64(left, above, above_left),
            Stage3Rule::Or128Tree32 => crate::lisa_or128_tree::context32(left, above, above_left),
        }
    }
    fn thresholds(self) -> [u32; 3] {
        match self {
            Stage3Rule::Legacy4 => STAGE3_THRESHOLDS,
            Stage3Rule::Or128 | Stage3Rule::Or128Tree | Stage3Rule::Or128Tree64 | Stage3Rule::Or128Tree32 => [0; 3],
        }
    }
    /// The rule a record describes, or `None` for an unknown version or a record that disagrees with its own rule.
    fn from_record(r: &Stage3Record) -> Option<Self> {
        [Stage3Rule::Legacy4, Stage3Rule::Or128, Stage3Rule::Or128Tree, Stage3Rule::Or128Tree64, Stage3Rule::Or128Tree32].into_iter().find(|k| {
            k.version() == r.rule_version && k.context_count() == usize::from(r.class_count) && k.thresholds() == r.thresholds
        })
    }
}

/// Local-activity class. Discriminants are stable and usable directly as a context index (0..=3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum ActivityClass {
    Low = 0,
    Moderate = 1,
    High = 2,
    VeryHigh = 3,
}

impl ActivityClass {
    pub const COUNT: usize = 4;

    #[inline]
    pub fn index(self) -> usize {
        self as usize
    }
}

/// Stage 3 reconstruction record. Describes the classification rule; it does not transform residual values.
/// `thresholds` is meaningful for the legacy rule only (all zero for OR-128).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stage3Record {
    pub rule_version: u8,
    pub class_count: u8,
    pub thresholds: [u32; 3],
}

impl Stage3Record {
    fn for_rule(rule: Stage3Rule) -> Self {
        Stage3Record { rule_version: rule.version(), class_count: rule.context_count() as u8, thresholds: rule.thresholds() }
    }
}

/// Classifies one residual from its causal neighbors (`None` = outside the plane or not yet available). Encoder and
/// decoder both call this with the already known zigzag residuals of left, above and upper-left.
#[inline]
pub fn classify_activity(left: Option<u32>, above: Option<u32>, above_left: Option<u32>) -> ActivityClass {
    let (mut sum, mut n) = (0u64, 0u64);
    for v in [left, above, above_left].into_iter().flatten() {
        sum += u64::from(v);
        n += 1;
    }
    if n == 0 {
        return ActivityClass::Low;
    }
    let score = (sum + n / 2) / n;
    let [t1, t2, t3] = STAGE3_THRESHOLDS.map(u64::from);
    if score < t1 {
        ActivityClass::Low
    } else if score < t2 {
        ActivityClass::Moderate
    } else if score < t3 {
        ActivityClass::High
    } else {
        ActivityClass::VeryHigh
    }
}

/// One context id (`0..Stage3Rule::context_count()`) per residual, laid out exactly like the residuals in
/// [`OrganizedResiduals`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrganizedContexts {
    /// Parallel to [`OrganizedResiduals::Morton`]. Neighbors are the spatial W, N, NW pixels, which always precede the
    /// current pixel in Morton order.
    Morton(Vec<u8>),
    /// Parallel to [`OrganizedResiduals::BayerPlanes`]. Neighbors are W, N, NW inside the same row-major plane.
    BayerPlanes([Vec<u8>; 4]),
}

/// Stage 3 output: the Stage 2 result (residuals, metadata and history, with the Stage 3 record appended) plus the
/// per-residual classes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stage3Output {
    base: Stage2Output,
    contexts: OrganizedContexts,
}

impl Stage3Output {
    /// Unchanged Stage 2 residuals.
    pub fn data(&self) -> &OrganizedResiduals {
        &self.base.data
    }
    pub fn contexts(&self) -> &OrganizedContexts {
        &self.contexts
    }
    /// The Stage 3 rule recorded in the newest history record.
    pub fn rule(&self) -> Option<Stage3Rule> {
        match self.base.history.last() {
            Some(ReconstructionRecord::Stage3Context(r)) => Stage3Rule::from_record(r),
            _ => None,
        }
    }
    pub fn metadata(&self) -> &ImageMetadata {
        &self.base.metadata
    }
    pub fn bit_depth(&self) -> u8 {
        self.base.bit_depth
    }
    /// Cumulative history, oldest first; the newest record is Stage 3.
    pub fn history(&self) -> &[ReconstructionRecord] {
        &self.base.history
    }
    /// Stage 2 organization mode (geometry for Stage 4), found beneath the Stage 3 record.
    pub fn mode(&self) -> Option<&Stage2Mode> {
        self.base.history.iter().rev().find_map(|r| match r {
            ReconstructionRecord::Stage2Organization(r) => Some(&r.mode),
            _ => None,
        })
    }
    /// Drops the classes and the Stage 3 record, returning the exact Stage 2 output.
    pub fn into_stage2(mut self) -> Stage2Output {
        self.base.history.pop();
        self.base
    }
}

fn classify_plane(rule: Stage3Rule, v: &[u32], w: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len());
    if w == 0 {
        return out;
    }
    for i in 0..v.len() {
        let (x, y) = (i % w, i / w);
        let left = (x > 0).then(|| v[i - 1]);
        let above = (y > 0).then(|| v[i - w]);
        let above_left = (x > 0 && y > 0).then(|| v[i - w - 1]);
        out.push(rule.context(left, above, above_left));
    }
    out
}

/// Stage 3 entry point (production rule, OR-128). Consumes the Stage 2 result, assigns a context to every residual and
/// appends a Stage 3 record.
pub fn classify_stage3(stage2: Stage2Output) -> Result<Stage3Output, LisaError> {
    classify_stage3_with(stage2, Stage3Rule::default())
}

/// [`classify_stage3`] with an explicit rule (the legacy four-class rule exists for old streams and control runs).
pub fn classify_stage3_with(stage2: Stage2Output, rule: Stage3Rule) -> Result<Stage3Output, LisaError> {
    stage2.metadata.validate()?;
    let (width, height) = (stage2.metadata.width, stage2.metadata.height);
    let n = validate(width, height, stage2.bit_depth)?;
    let Some(ReconstructionRecord::Stage2Organization(rec)) = stage2.history.last() else {
        return Err(LisaError::InvalidMetadata("newest reconstruction record is not Stage 2"));
    };
    if (rec.width, rec.height) != (width, height) {
        return Err(LisaError::MetadataGeometryMismatch { expected: (width, height), actual: (rec.width, rec.height) });
    }

    let contexts = match (&rec.mode, &stage2.data) {
        (Stage2Mode::Identity, OrganizedResiduals::Morton(r)) => {
            if r.len() != n {
                return Err(LisaError::LengthMismatch { expected: n, actual: r.len() });
            }
            // Row-major scratch of residuals already visited; W, N and NW precede the current pixel in Morton order.
            let mut seen = vec![0u32; n];
            let mut out = Vec::with_capacity(n);
            let mut k = 0usize;
            morton_visit::<LisaError>(width, height, |x, y| {
                let left = (x > 0).then(|| seen[y * width + x - 1]);
                let above = (y > 0).then(|| seen[(y - 1) * width + x]);
                let above_left = (x > 0 && y > 0).then(|| seen[(y - 1) * width + x - 1]);
                out.push(rule.context(left, above, above_left));
                seen[y * width + x] = r[k];
                k += 1;
                Ok(())
            })?;
            OrganizedContexts::Morton(out)
        }
        (Stage2Mode::BayerPlanes { planes, .. }, OrganizedResiduals::BayerPlanes(data)) => {
            let mut out: [Vec<u8>; 4] = Default::default();
            for ((o, v), p) in out.iter_mut().zip(data).zip(planes) {
                let len = p.width.checked_mul(p.height).ok_or(LisaError::DimensionTooLarge)?;
                if v.len() != len {
                    return Err(LisaError::LengthMismatch { expected: len, actual: v.len() });
                }
                *o = classify_plane(rule, v, p.width);
            }
            OrganizedContexts::BayerPlanes(out)
        }
        _ => return Err(LisaError::InvalidMetadata("organized data does not match the Stage 2 record")),
    };

    let mut base = stage2;
    base.history.push(ReconstructionRecord::Stage3Context(Stage3Record::for_rule(rule)));
    Ok(Stage3Output { base, contexts })
}

// ---------------------------------------------------------------- Stage 4: PKON context-adaptive entropy coding
//
// Each residual is coded with the PKON binary range coder and one of the independent adaptive models chosen by its
// Stage 3 context (4 for the legacy rule, 128 for OR-128). A residual's zigzag value v is binarized as x = v + 1 (1..=2^32): the bit length e of x in unary
// (adaptive, one probability per position), then the e-1 bits below the leading one, MSB first; the first of those is
// adaptive per e and the rest are coded with fixed p = 1/2. Models start fresh (p = 1/2, count 0), are never shared or
// reset between planes, and only the selected model adapts. The decoder recomputes each class from already decoded
// residuals with the Stage 3 rule, so no class labels are stored. Traversal is the Stage 3 order: Bayer planes 0..3 in
// row-major order, or the Morton visitation order for the identity path.

/// Coding-mode/version of the Stage 4 stream defined here: four class-selected adaptive models, binarization above.
pub const STAGE4_CODING_VERSION: u8 = 1;
/// EXPERIMENTAL coding-mode/version: identical binarization and model selection, but the reward-penalty (RP1, α = 1/32)
/// probability estimator from `lisa_rp`. Never produced unless [`Stage4Estimator::RewardPenalty`] is requested.
pub const STAGE4_RP1_CODING_VERSION: u8 = 2;

/// Probability estimator used by Stage 4. `Counted` is the production default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Stage4Estimator {
    #[default]
    Counted,
    RewardPenalty,
}
const S4_E_MAX: usize = 33;

/// Stage 4 reconstruction record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stage4Record {
    pub coding_version: u8,
    pub model_count: u8,
    /// Number of residuals coded (redundant with the geometry; validated on decode).
    pub residual_count: u64,
}

#[derive(Clone, Copy)]
struct S4Model<E: Est> {
    unary: [E; S4_E_MAX + 1],
    mant: [E; S4_E_MAX + 1],
}

impl<E: Est> S4Model<E> {
    fn new() -> Self {
        S4Model { unary: [E::fresh(); S4_E_MAX + 1], mant: [E::fresh(); S4_E_MAX + 1] }
    }

    /// Codes one adaptive bit and updates it. Encoders pass the real `bit`; decoders pass anything and get the decoded one.
    #[inline]
    fn bit<I: BitIo>(io: &mut I, b: &mut E, bit: u32) -> u32 {
        let bit = io.code(b.p16(), bit);
        b.update(bit);
        bit
    }

    /// Shared encode/decode of one zigzag residual; returns the (de)coded value.
    fn code<I: BitIo>(&mut self, io: &mut I, v: u32) -> Result<u32, LisaError> {
        let x = u64::from(v) + 1;
        let e_in = (64 - x.leading_zeros()) as usize;
        let mut e = S4_E_MAX;
        for i in 1..S4_E_MAX {
            if Self::bit(io, &mut self.unary[i], (e_in > i) as u32) == 0 {
                e = i;
                break;
            }
        }
        let mut out = 1u64;
        for j in (0..e - 1).rev() {
            let want = ((x >> j) & 1) as u32;
            let b = if j == e - 2 { Self::bit(io, &mut self.mant[e], want) } else { io.code(32768, want) };
            out = (out << 1) | u64::from(b);
        }
        u32::try_from(out - 1).map_err(|_| LisaError::CorruptStream("residual exceeds 32 bits"))
    }
}

/// Stage 4 output: the entropy-coded Stage 3 residuals plus the inherited metadata and cumulative history (newest
/// record is Stage 4). Contexts are not stored; they are rebuilt while decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stage4Output {
    payload: Vec<u8>,
    metadata: ImageMetadata,
    bit_depth: u8,
    history: Vec<ReconstructionRecord>,
}

impl Stage4Output {
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }
    pub fn metadata(&self) -> &ImageMetadata {
        &self.metadata
    }
    pub fn bit_depth(&self) -> u8 {
        self.bit_depth
    }
    /// Cumulative history, oldest first; the newest record is Stage 4.
    pub fn history(&self) -> &[ReconstructionRecord] {
        &self.history
    }

    /// Container deserialization support (`pkon`): rebuilds a Stage 4 result from a stored payload plus the few
    /// parameters a file carries. The history is derived exactly as the encoder records it (Stage 2 mode follows
    /// `metadata.layout`: `BayerCfa` -> `BayerPlanes`, everything else `Identity`), so `decode_stage4` validates it
    /// like any in-memory stream. No coding or prediction logic lives here.
    pub fn from_parts(
        payload: Vec<u8>,
        metadata: ImageMetadata,
        bit_depth: u8,
        planar_stage1: bool,
        rule: Stage3Rule,
        coding_version: u8,
    ) -> Result<Self, LisaError> {
        metadata.validate()?;
        let (width, height) = (metadata.width, metadata.height);
        let n = validate(width, height, bit_depth)?;
        let mode = match (metadata.layout, &metadata.cfa) {
            (SampleLayout::BayerCfa, Some(cfa)) => {
                Stage2Mode::BayerPlanes { cfa: cfa.clone(), planes: bayer_planes(width, height, cfa)? }
            }
            _ => Stage2Mode::Identity,
        };
        let stage1 = if planar_stage1 {
            ReconstructionRecord::Stage1BayerPlanar { bit_depth }
        } else {
            ReconstructionRecord::Stage1Decorrelation { bit_depth }
        };
        let history = vec![
            stage1,
            ReconstructionRecord::Stage2Organization(Stage2Record { width, height, mode }),
            ReconstructionRecord::Stage3Context(Stage3Record::for_rule(rule)),
            ReconstructionRecord::Stage4Entropy(Stage4Record {
                coding_version,
                model_count: rule.context_count() as u8,
                residual_count: n as u64,
            }),
        ];
        Ok(Stage4Output { payload, metadata, bit_depth, history })
    }
}

/// Stage 4 entry point: entropy-codes the Stage 3 residuals, selecting the model by each residual's Stage 3 class.
pub fn encode_stage4(stage3: Stage3Output) -> Result<Stage4Output, LisaError> {
    encode_stage4_with(stage3, Stage4Estimator::Counted)
}

/// [`encode_stage4`] with an explicit estimator. `Counted` is byte-identical to `encode_stage4`; `RewardPenalty` is
/// experimental and is recorded in the stream as [`STAGE4_RP1_CODING_VERSION`].
pub fn encode_stage4_with(stage3: Stage3Output, estimator: Stage4Estimator) -> Result<Stage4Output, LisaError> {
    match estimator {
        Stage4Estimator::Counted => encode_s4::<Bit>(stage3, STAGE4_CODING_VERSION),
        Stage4Estimator::RewardPenalty => encode_s4::<Rp>(stage3, STAGE4_RP1_CODING_VERSION),
    }
}

fn encode_s4<E: Est>(stage3: Stage3Output, coding_version: u8) -> Result<Stage4Output, LisaError> {
    let (width, height) = (stage3.base.metadata.width, stage3.base.metadata.height);
    validate(width, height, stage3.base.bit_depth)?;
    let rule = stage3
        .rule()
        .ok_or(LisaError::InvalidMetadata("newest reconstruction record is not a supported Stage 3 record"))?;
    let mut count = 0u64;
    let mut models = vec![S4Model::<E>::new(); rule.context_count()];
    let mut enc = BinEnc::new();
    let mut code_all = |res: &[u32], ctx: &[u8]| -> Result<(), LisaError> {
        if res.len() != ctx.len() {
            return Err(LisaError::LengthMismatch { expected: res.len(), actual: ctx.len() });
        }
        count += res.len() as u64;
        for (&v, &c) in res.iter().zip(ctx) {
            models.get_mut(usize::from(c)).ok_or(LisaError::InvalidMetadata("context id out of range"))?.code(&mut enc, v)?;
        }
        Ok(())
    };
    match (&stage3.base.data, &stage3.contexts) {
        (OrganizedResiduals::Morton(r), OrganizedContexts::Morton(c)) => code_all(r, c)?,
        (OrganizedResiduals::BayerPlanes(r), OrganizedContexts::BayerPlanes(c)) => {
            for (r, c) in r.iter().zip(c) {
                code_all(r, c)?;
            }
        }
        _ => return Err(LisaError::InvalidMetadata("contexts do not match the organized data")),
    }
    let payload = enc.finish();
    let Stage2Output { metadata, bit_depth, mut history, .. } = stage3.base;
    history.push(ReconstructionRecord::Stage4Entropy(Stage4Record {
        coding_version,
        model_count: rule.context_count() as u8,
        residual_count: count,
    }));
    Ok(Stage4Output { payload, metadata, bit_depth, history })
}

/// Exact inverse of [`encode_stage4`]: returns the Stage 3 representation (residuals, contexts, history without the
/// Stage 4 record). Feed it to [`Stage3Output::into_stage2`] and [`restore_stage1`] to continue reversing.
pub fn decode_stage4(out: &Stage4Output) -> Result<Stage3Output, LisaError> {
    let Some((ReconstructionRecord::Stage4Entropy(rec), _)) = out.history.split_last() else {
        return Err(LisaError::InvalidMetadata("newest reconstruction record is not Stage 4"));
    };
    match rec.coding_version {
        STAGE4_CODING_VERSION => decode_s4::<Bit>(out),
        STAGE4_RP1_CODING_VERSION => decode_s4::<Rp>(out),
        _ => Err(LisaError::CorruptStream("unsupported Stage 4 coding mode or version")),
    }
}

fn decode_s4<E: Est>(out: &Stage4Output) -> Result<Stage3Output, LisaError> {
    let Some((ReconstructionRecord::Stage4Entropy(rec), inherited)) = out.history.split_last() else {
        return Err(LisaError::InvalidMetadata("newest reconstruction record is not Stage 4"));
    };
    // The Stage 3 record, not the Stage 4 one, selects the context rule; the model count must agree with it.
    let Some(rule) = (match inherited.last() {
        Some(ReconstructionRecord::Stage3Context(r)) => Stage3Rule::from_record(r),
        _ => None,
    }) else {
        return Err(LisaError::InvalidMetadata("Stage 4 does not follow a supported Stage 3 record"));
    };
    if usize::from(rec.model_count) != rule.context_count() {
        return Err(LisaError::CorruptStream("unsupported Stage 4 coding mode or version"));
    }
    out.metadata.validate()?;
    let (width, height) = (out.metadata.width, out.metadata.height);
    let n = validate(width, height, out.bit_depth)?;
    let Some(ReconstructionRecord::Stage2Organization(s2)) =
        inherited.iter().rev().find(|r| matches!(r, ReconstructionRecord::Stage2Organization(_)))
    else {
        return Err(LisaError::InvalidMetadata("missing Stage 2 record"));
    };
    if (s2.width, s2.height) != (width, height) {
        return Err(LisaError::MetadataGeometryMismatch { expected: (width, height), actual: (s2.width, s2.height) });
    }
    if rec.residual_count != n as u64 {
        return Err(LisaError::CorruptStream("residual count disagrees with geometry"));
    }

    let mut models = vec![S4Model::<E>::new(); rule.context_count()];
    let mut dec = BinDec::new(&out.payload);
    // `c` always comes from `Stage3Rule::context`, which is below `context_count()`.
    let mut next = |c: u8| models[usize::from(c)].code(&mut dec, 0);

    let (data, contexts) = match &s2.mode {
        Stage2Mode::Identity => {
            let mut seen = vec![0u32; n];
            let (mut res, mut ctx) = (Vec::with_capacity(n), Vec::with_capacity(n));
            morton_visit::<LisaError>(width, height, |x, y| {
                let left = (x > 0).then(|| seen[y * width + x - 1]);
                let above = (y > 0).then(|| seen[(y - 1) * width + x]);
                let above_left = (x > 0 && y > 0).then(|| seen[(y - 1) * width + x - 1]);
                let c = rule.context(left, above, above_left);
                let v = next(c)?;
                seen[y * width + x] = v;
                res.push(v);
                ctx.push(c);
                Ok(())
            })?;
            (OrganizedResiduals::Morton(res), OrganizedContexts::Morton(ctx))
        }
        Stage2Mode::BayerPlanes { cfa, planes } => {
            if out.metadata.cfa.as_ref() != Some(cfa) || bayer_planes(width, height, cfa)? != *planes {
                return Err(LisaError::InvalidMetadata("Stage 2 record disagrees with metadata"));
            }
            let mut res: [Vec<u32>; 4] = Default::default();
            let mut ctx: [Vec<u8>; 4] = Default::default();
            for ((r, c), p) in res.iter_mut().zip(ctx.iter_mut()).zip(planes) {
                let (w, len) = (p.width, p.width.checked_mul(p.height).ok_or(LisaError::DimensionTooLarge)?);
                r.reserve_exact(len);
                c.reserve_exact(len);
                for i in 0..len {
                    let (x, y) = (i % w, i / w);
                    let left = (x > 0).then(|| r[i - 1]);
                    let above = (y > 0).then(|| r[i - w]);
                    let above_left = (x > 0 && y > 0).then(|| r[i - w - 1]);
                    let cl = rule.context(left, above, above_left);
                    r.push(next(cl)?);
                    c.push(cl);
                }
            }
            (OrganizedResiduals::BayerPlanes(res), OrganizedContexts::BayerPlanes(ctx))
        }
    };
    if dec.consumed() != out.payload.len() {
        return Err(LisaError::CorruptStream("payload truncated or has trailing bytes"));
    }
    let base = Stage2Output { data, metadata: out.metadata.clone(), bit_depth: out.bit_depth, history: inherited.to_vec() };
    Ok(Stage3Output { base, contexts })
}

#[cfg(test)]
mod or128_stream_tests {
    //! Malformed / mismatched stream handling for the versioned Stage 3 rule and the Stage 4 model count.
    use super::*;

    fn s4_of(rule: Stage3Rule) -> Stage4Output {
        let samples: Vec<u16> = (0..40 * 30).map(|i: u32| ((i * 37 + (i / 7) * 11) % 1024) as u16).collect();
        let meta = ImageMetadata::without_cfa(40, 30, SampleLayout::Monochrome);
        let s1 = encode_with_metadata(&samples, 40, 30, 10, meta).unwrap();
        encode_stage4(classify_stage3_with(organize_stage2(s1).unwrap(), rule).unwrap()).unwrap()
    }

    fn tamper(mut o: Stage4Output, f: impl FnOnce(&mut Stage3Record, &mut Stage4Record)) -> Stage4Output {
        let n = o.history.len();
        let (a, b) = o.history.split_at_mut(n - 1);
        let (ReconstructionRecord::Stage3Context(r3), ReconstructionRecord::Stage4Entropy(r4)) = (&mut a[n - 2], &mut b[0]) else {
            panic!("unexpected history")
        };
        f(r3, r4);
        o
    }

    #[test]
    fn valid_streams_decode() {
        for rule in [Stage3Rule::Legacy4, Stage3Rule::Or128, Stage3Rule::Or128Tree] {
            assert!(decode_stage4(&s4_of(rule)).is_ok());
        }
    }

    #[test]
    fn rule_cannot_be_reinterpreted() {
        for (from, to) in [
            (Stage3Rule::Legacy4, Stage3Rule::Or128),
            (Stage3Rule::Or128, Stage3Rule::Legacy4),
            (Stage3Rule::Or128, Stage3Rule::Or128Tree),
            (Stage3Rule::Or128Tree, Stage3Rule::Or128),
            (Stage3Rule::Legacy4, Stage3Rule::Or128Tree),
        ] {
            // Relabelling only the version is inconsistent with the record's own counts: rejected.
            assert!(decode_stage4(&tamper(s4_of(from), |r3, _| r3.rule_version = to.version())).is_err());
            // A fully consistent relabel is a different valid stream; it must not reproduce the original residuals.
            let full = tamper(s4_of(from), |r3, r4| {
                *r3 = Stage3Record::for_rule(to);
                r4.model_count = to.context_count() as u8;
            });
            let orig = decode_stage4(&s4_of(from)).unwrap();
            if let Ok(d) = decode_stage4(&full) {
                assert_ne!(d.data(), orig.data());
            }
        }
    }

    #[test]
    fn invalid_versions_and_counts_fail_safely() {
        let ok = s4_of(Stage3Rule::Or128);
        for bad in [0u8, 6, 255] {
            assert!(decode_stage4(&tamper(ok.clone(), |r3, _| r3.rule_version = bad)).is_err(), "rule {bad}");
        }
        for bad in [0u8, 4, 127, 129, 255] {
            assert!(decode_stage4(&tamper(ok.clone(), |_, r4| r4.model_count = bad)).is_err(), "model_count {bad}");
            assert!(decode_stage4(&tamper(ok.clone(), |r3, _| r3.class_count = bad)).is_err(), "class_count {bad}");
        }
        assert!(decode_stage4(&tamper(ok.clone(), |r3, _| r3.thresholds = [3, 12, 48])).is_err());
        assert!(decode_stage4(&tamper(ok, |_, r4| r4.coding_version = 9)).is_err());
        let legacy = s4_of(Stage3Rule::Legacy4);
        assert!(decode_stage4(&tamper(legacy.clone(), |r3, _| r3.thresholds = [3, 12, 49])).is_err());
        assert!(decode_stage4(&tamper(legacy, |_, r4| r4.model_count = 128)).is_err());
    }

    #[test]
    fn truncated_or_extended_payload_is_rejected() {
        let ok = s4_of(Stage3Rule::Or128);
        let mut short = ok.clone();
        short.payload.truncate(short.payload.len() / 2);
        assert!(decode_stage4(&short).is_err());
        let mut long = ok;
        long.payload.push(0);
        assert!(decode_stage4(&long).is_err());
    }
}
