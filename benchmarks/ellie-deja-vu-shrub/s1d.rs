//! Deja Vu Shrub: copy of the production ELLIE Stage 1 scan (src/ellie.rs) with adaptive H/V/D weighting added.
use nanoscope::ellie::{Stage1Error, MAX_BITS};

#[inline(always)]
fn paeth(a: i32, b: i32, c: i32) -> i32 {
    let p = a + b - c;
    let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// Same H/V/D estimates and Paeth refinement as production; the only change is that each directional weight is also
/// multiplied by an adaptive factor `f` (learned from past estimate errors). Returns (prediction, 4x estimates incl. offset).
#[inline(always)]
fn predict(m: i32, w: i32, ww: i32, n: i32, nn: i32, nw: i32, ne: i32, s_h: u32, s_v: u32, s45: u32, s135: u32, f: [u64; 3], paeth_on: bool) -> (i32, [u64; 3]) {
    let mn = s45.min(s135);
    let s_d = mn + (mn >> 2);
    let smax = s_h.max(s_v).max(s_d);
    let sh = (32 - (smax | 1).leading_zeros()).saturating_sub(10);
    let sq = |s: u32| {
        let r = ((s >> sh) + 1) as u64;
        r * r
    };
    let (qh, qv, qd) = (sq(s_h), sq(s_v), sq(s_d));
    let (ph, pv, pd) = ((qv * qd) as u128 * f[0] as u128, (qh * qd) as u128 * f[1] as u128, (qh * qv) as u128 * f[2] as u128);
    let d = ph + pv + pd;

    let fq = 2 + (s135 >= 2 * s45) as i32 - (s45 >= 2 * s135) as i32;
    let o = 4 * m;
    let h4 = (4 * w + ((w - ww + n - nw) >> 1) + o) as u64;
    let v4 = (4 * n + ((n - nn + w - nw) >> 1) + o) as u64;
    let base4 = (3 * ((4 - fq) * nw + fq * ne)) / 4 + (w + ne - n) + o;
    let d4 = if paeth_on { ((base4 + 4 * paeth(w, n, nw) + o + 1) >> 1) as u64 } else { base4 as u64 };

    let t = ((ph * h4 as u128 + pv * v4 as u128 + pd * d4 as u128) / d) as u64;
    ((((t + 2) >> 2) as i64 - m as i64).clamp(0, m as i64) as i32, [h4, v4, d4])
}

/// Adaptive weight state (Deja Vu Shrub): per-estimator running mean of |4x - 4*estimate|, fixed-point x16, rate 1/16.
/// Factor f_i = 256 * (Emin + C) / (E_i + C), floored at 1. Updated after each pixel is known to both sides.
#[derive(Clone, Copy)]
struct Dvs {
    e: [i64; 3],
    c: i64,
    shift: u32,
    on: bool,
}
#[derive(Clone, Copy)]
pub struct Cfg {
    pub shrub: bool,
    pub shift: u32,
    pub c: i64,
    pub paeth: bool,
}
impl Dvs {
    #[inline(always)]
    fn factors(&self) -> [u64; 3] {
        if !self.on {
            return [256; 3];
        }
        let mn = self.e[0].min(self.e[1]).min(self.e[2]);
        let c = self.c;
        let f = |e: i64| ((256 * (mn + c)) / (e + c)).max(1) as u64;
        [f(self.e[0]), f(self.e[1]), f(self.e[2])]
    }
    #[inline(always)]
    fn update(&mut self, x: i32, est4: [u64; 3], m: i32) {
        if !self.on {
            return;
        }
        let t = 4 * x as i64 + 4 * m as i64;
        for k in 0..3 {
            let err = (t - est4[k] as i64).abs() << 4;
            self.e[k] += (err - self.e[k]) >> self.shift;
        }
    }
}

/// Reusable Stage 1 scanner for one plane geometry. All buffers are allocated once in [`Stage1::new`].
pub struct Stage1D {
    w: usize,
    h: usize,
    bits: u32,
    cfg: Cfg,
    m: u32,
    // Three padded pixel rows (2 pad columns each side), rotated cur -> p -> pp.
    ra: Vec<u16>,
    rb: Vec<u16>,
    rc: Vec<u16>,
    // Two difference rows `[H, V, A, G]`, index = column, plus one ghost column at `w`.
    da: Vec<[u16; 4]>,
    db: Vec<[u16; 4]>,
}

impl Stage1D {
    pub fn new(width: usize, height: usize, bits: u32, cfg: Cfg) -> Result<Self, Stage1Error> {
        if !(1..=MAX_BITS).contains(&bits) {
            return Err(Stage1Error::BadBits(bits));
        }
        Ok(Self {
            w: width,
            h: height,
            bits,
            cfg,
            m: (1u32 << bits) - 1,
            ra: vec![0; width + 4],
            rb: vec![0; width + 4],
            rc: vec![0; width + 4],
            da: vec![[0; 4]; width + 1],
            db: vec![[0; 4]; width + 1],
        })
    }

    pub fn max_value(&self) -> u32 {
        self.m
    }

    /// Shared raster scan. `resolve(index, prediction)` returns the actual sample (known to the encoder,
    /// reconstructed by the decoder); everything else is identical, so both sides cannot diverge.
    #[inline(always)]
    pub fn scan<F: FnMut(usize, u32) -> u32>(&mut self, mut resolve: F) {
        let (w, h, m) = (self.w, self.h, self.m as i32);
        if w == 0 || h == 0 {
            return;
        }
        let paeth_on = self.cfg.paeth;
        let mut dvs = Dvs { e: [0; 3], c: self.cfg.c, shift: self.cfg.shift, on: self.cfg.shrub };
        let mid = 1u32 << (self.bits - 1);
        let mut cur: &mut [u16] = &mut self.ra[..];
        let mut p: &mut [u16] = &mut self.rb[..];
        let mut pp: &mut [u16] = &mut self.rc[..];
        let mut dcur: &mut [[u16; 4]] = &mut self.da[..];
        let mut dprev: &mut [[u16; 4]] = &mut self.db[..];

        // ---- Row 0: scalar horizontal loop; pixel 0 from mid-range. ----
        let x0 = resolve(0, mid) as u16;
        cur[2] = x0;
        cur[0] = x0;
        cur[1] = x0;
        for x in 1..w {
            let wv = cur[x + 1] as i32;
            let wwv = cur[x] as i32; // x = 1 reads the pad, which equals W
            let pr = ((5 * wv - wwv + 2) >> 2).clamp(0, m);
            cur[x + 2] = resolve(x, pr as u32) as u16;
        }
        let last = cur[w + 1];
        cur[w + 2] = last;
        cur[w + 3] = last;
        // Difference row of row 0 against a replicated row -1: V = 0, A = G = H.
        dcur[0] = [0; 4];
        for x in 1..w {
            let hh = (cur[x + 2] as i32 - cur[x + 1] as i32).unsigned_abs() as u16;
            dcur[x] = [hh, 0, hh, hh];
        }
        dcur[w] = [0; 4];
        pp.copy_from_slice(cur);
        std::mem::swap(&mut p, &mut cur); // p = row 0, cur = scratch
        std::mem::swap(&mut dprev, &mut dcur);

        // ---- Rows >= 1. ----
        for y in 1..h {
            let base = y * w;
            let n0 = p[2];
            cur[0] = n0;
            cur[1] = n0;

            // x = 0: W = WW = NW = N.
            let (mut h_c, mut v_c, mut a_c);
            {
                let n = n0 as i32;
                let (nn, ne) = (pp[2] as i32, p[3] as i32);
                let (d0, d1) = (dprev[0], dprev[1]);
                let s_h = d0[0] as u32 + d1[0] as u32;
                let s_v = d0[1] as u32 + d1[1] as u32;
                let s45 = d0[2] as u32;
                let s135 = d1[3] as u32 + d0[3] as u32;
                let (pr, est) = predict(m, n, n, n, nn, n, ne, s_h, s_v, s45, s135, dvs.factors(), paeth_on);
                let xv = resolve(base, pr as u32);
                dvs.update(xv as i32, est, m);
                let v = (xv as i32 - n).unsigned_abs() as u16;
                cur[2] = xv as u16;
                cur[0] = xv as u16;
                cur[1] = xv as u16;
                dcur[0] = [0, v, v, v];
                h_c = 0u32;
                v_c = v as u32;
                a_c = (xv as i32 - ne).unsigned_abs();
            }

            for x in 1..w {
                let i = x + 2;
                let wv = cur[i - 1] as i32;
                let wwv = cur[i - 2] as i32;
                let n = p[i] as i32;
                let nw = p[i - 1] as i32;
                let ne = p[i + 1] as i32;
                let nn = pp[i] as i32;
                let (d0, d1) = (dprev[x], dprev[x + 1]);
                let s_h = h_c + d0[0] as u32 + d1[0] as u32;
                let s_v = v_c + d0[1] as u32 + d1[1] as u32;
                let s45 = a_c + d0[2] as u32;
                let s135 = d1[3] as u32 + d0[3] as u32;
                let (pr, est) = predict(m, wv, wwv, n, nn, nw, ne, s_h, s_v, s45, s135, dvs.factors(), paeth_on);
                let xv = resolve(base + x, pr as u32) as i32;
                dvs.update(xv, est, m);
                let hh = (xv - wv).unsigned_abs();
                let vv = (xv - n).unsigned_abs();
                let gg = (xv - nw).unsigned_abs();
                let an = (xv - ne).unsigned_abs();
                dcur[x] = [hh as u16, vv as u16, a_c as u16, gg as u16];
                cur[i] = xv as u16;
                h_c = hh;
                v_c = vv;
                a_c = an;
            }

            // Ghost column and right pads (replicate the last pixel).
            let last = cur[w + 1];
            cur[w + 2] = last;
            cur[w + 3] = last;
            dcur[w] = [0, v_c as u16, 0, v_c as u16];

            // Rotate (pp, p, cur) <- (p, cur, pp).
            std::mem::swap(&mut pp, &mut p);
            std::mem::swap(&mut p, &mut cur);
            std::mem::swap(&mut dprev, &mut dcur);
        }
    }
}

