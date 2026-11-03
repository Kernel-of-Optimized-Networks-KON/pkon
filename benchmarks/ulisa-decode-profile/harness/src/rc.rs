//! Experimental range-decoder variants (harness only; production `Stage4Dec` in src/ellie.rs is untouched) plus the shared
//! driver that runs the production Stage 1 / Stage 3 decode loop with any of them as the `BitIo` back-end.
//!
//! All variants implement exactly the arithmetic of `Stage4Dec::code`: 32-bit range in [2^24, 2^32), `bound = (range * p1) >> 16`,
//! bit 1 takes the lower part, bytes past the end read as zero.
use nanoscope::ellie::{flank_base, FlankModel, FlankTopology, Stage1, Stage3, Stage3Profile, Stage4Dec};
use nanoscope::lisa_frame::BayerFrame;
use nanoscope::pkon::{self, CodecParams, Decoded};
use nanoscope::rangecoder::BitIo;

const TOP: u32 = 1 << 24;

/// B: verbatim copy of `Stage4Dec` (control for "copy in this crate == production").
pub struct Copy0<'a> {
    pub code: u32,
    pub range: u32,
    pub buf: &'a [u8],
    pub pos: usize,
}
impl<'a> Copy0<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        let mut code = 0u32;
        for i in 0..4 {
            code = (code << 8) | buf.get(i).copied().unwrap_or(0) as u32;
        }
        Self { code, range: u32::MAX, buf, pos: 4 }
    }
}
impl BitIo for Copy0<'_> {
    #[inline(always)]
    fn code(&mut self, p1: u32, _bit: u32) -> u32 {
        let bound = ((self.range as u64 * p1 as u64) >> 16) as u32;
        let bit = if self.code < bound {
            self.range = bound;
            1
        } else {
            self.code -= bound;
            self.range -= bound;
            0
        };
        while self.range < TOP {
            self.code = (self.code << 8) | self.buf.get(self.pos).copied().unwrap_or(0) as u32;
            self.pos += 1;
            self.range <<= 8;
        }
        bit
    }
}

/// C: branch-free bit/range resolution (mask arithmetic); renormalisation loop unchanged.
pub struct Bf<'a>(pub Copy0<'a>);
impl BitIo for Bf<'_> {
    #[inline(always)]
    fn code(&mut self, p1: u32, _bit: u32) -> u32 {
        let s = &mut self.0;
        let bound = ((s.range as u64 * p1 as u64) >> 16) as u32;
        let lt = (s.code < bound) as u32;
        let m = 0u32.wrapping_sub(lt);
        s.code = s.code.wrapping_sub(bound & !m);
        s.range = (bound & m) | (s.range.wrapping_sub(bound) & !m);
        while s.range < TOP {
            s.code = (s.code << 8) | s.buf.get(s.pos).copied().unwrap_or(0) as u32;
            s.pos += 1;
            s.range <<= 8;
        }
        lt
    }
}

/// E: original branchy resolution plus a dedicated p = 1/2 path (`bound = range >> 1`, at most one renormalisation byte).
pub struct Raw0<'a>(pub Copy0<'a>);
impl BitIo for Raw0<'_> {
    #[inline(always)]
    fn code(&mut self, p1: u32, _bit: u32) -> u32 {
        let s = &mut self.0;
        let bound = if p1 == 32768 { s.range >> 1 } else { ((s.range as u64 * p1 as u64) >> 16) as u32 };
        let bit = if s.code < bound {
            s.range = bound;
            1
        } else {
            s.code -= bound;
            s.range -= bound;
            0
        };
        while s.range < TOP {
            s.code = (s.code << 8) | s.buf.get(s.pos).copied().unwrap_or(0) as u32;
            s.pos += 1;
            s.range <<= 8;
        }
        bit
    }
}

/// G: branch-free resolution (as C) plus the dedicated p = 1/2 path (as E); original renormalisation loop and bounds-checked refill.
pub struct BfRaw<'a>(pub Copy0<'a>);
impl BitIo for BfRaw<'_> {
    #[inline(always)]
    fn code(&mut self, p1: u32, _bit: u32) -> u32 {
        let s = &mut self.0;
        let bound = if p1 == 32768 { s.range >> 1 } else { ((s.range as u64 * p1 as u64) >> 16) as u32 };
        let lt = (s.code < bound) as u32;
        let m = 0u32.wrapping_sub(lt);
        s.code = s.code.wrapping_sub(bound & !m);
        s.range = (bound & m) | (s.range.wrapping_sub(bound) & !m);
        while s.range < TOP {
            s.code = (s.code << 8) | s.buf.get(s.pos).copied().unwrap_or(0) as u32;
            s.pos += 1;
            s.range <<= 8;
        }
        lt
    }
}

/// Padded input: the payload followed by zero bytes, so a refill never needs an end-of-buffer test (reads clamp to the padding).
pub struct Padded<'a> {
    pub code: u32,
    pub range: u32,
    pub buf: &'a [u8],
    pub lim: usize,
    pub pos: usize,
}
impl<'a> Padded<'a> {
    /// `padded` = payload + at least 8 zero bytes.
    pub fn new(padded: &'a [u8]) -> Self {
        let code = u32::from_be_bytes([padded[0], padded[1], padded[2], padded[3]]);
        Self { code, range: u32::MAX, buf: padded, lim: padded.len() - 2, pos: 4 }
    }
    /// Renormalise by 0, 1 or 2 bytes without a loop. After a decision `range >= 2^13`, so two bytes always suffice.
    #[inline(always)]
    fn refill2(&mut self) {
        let n = (self.range < TOP) as u32 + (self.range < (1 << 16)) as u32;
        let i = self.pos.min(self.lim);
        let w = ((self.buf[i] as u32) << 8) | self.buf[i + 1] as u32;
        let sh = 8 * n;
        self.code = (self.code << sh) | (w >> (16 - sh));
        self.range <<= sh;
        self.pos += n as usize;
    }
    /// After a p = 1/2 decision `range >= 2^23`: at most one byte.
    #[inline(always)]
    fn refill1(&mut self) {
        let n = (self.range < TOP) as u32;
        let i = self.pos.min(self.lim);
        let sh = 8 * n;
        self.code = (self.code << sh) | ((self.buf[i] as u32) >> (8 - sh));
        self.range <<= sh;
        self.pos += n as usize;
    }
    #[inline(always)]
    fn resolve(&mut self, bound: u32) -> u32 {
        let lt = (self.code < bound) as u32;
        let m = 0u32.wrapping_sub(lt);
        self.code = self.code.wrapping_sub(bound & !m);
        self.range = (bound & m) | (self.range.wrapping_sub(bound) & !m);
        lt
    }
}

/// D: branch-free resolution + branch-free, bounds-check-free refill.
pub struct BfPd<'a>(pub Padded<'a>);
impl BitIo for BfPd<'_> {
    #[inline(always)]
    fn code(&mut self, p1: u32, _bit: u32) -> u32 {
        let s = &mut self.0;
        let bound = ((s.range as u64 * p1 as u64) >> 16) as u32;
        let bit = s.resolve(bound);
        s.refill2();
        bit
    }
}

/// F: D plus the dedicated p = 1/2 path.
pub struct BfPdRaw<'a>(pub Padded<'a>);
impl BitIo for BfPdRaw<'_> {
    #[inline(always)]
    fn code(&mut self, p1: u32, _bit: u32) -> u32 {
        let s = &mut self.0;
        if p1 == 32768 {
            let bit = s.resolve(s.range >> 1);
            s.refill1();
            bit
        } else {
            let bound = ((s.range as u64 * p1 as u64) >> 16) as u32;
            let bit = s.resolve(bound);
            s.refill2();
            bit
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum V {
    /// production `Stage4Dec`, driven by this harness
    A,
    B,
    C,
    D,
    E,
    F,
    G,
}
pub const ALL: [V; 7] = [V::A, V::B, V::C, V::D, V::E, V::F, V::G];
impl V {
    pub fn name(self) -> &'static str {
        match self {
            V::A => "A production Stage4Dec",
            V::B => "B verbatim copy",
            V::C => "C branch-free resolve",
            V::D => "D C+branch-free padded refill",
            V::E => "E raw p=1/2 path (branchy)",
            V::F => "F D+raw p=1/2 path",
            V::G => "G C+raw p=1/2 path (loop refill)",
        }
    }
}

/// Reference data for one file, built once from the production decoder (CRC-checked inside `pkon::decode`).
pub struct Prep {
    pub frame: BayerFrame,
    pub topo: FlankTopology,
    pub dims: [(usize, usize); 4],
    pub depth: u32,
    pub bit_depth: u8,
    pub crc: u32,
    pub payload: Vec<u8>,
    pub padded: Vec<u8>,
    pub bases: Vec<Vec<u8>>,
    pub ref_samples: Vec<u16>,
    pub samples: f64,
}

fn bases_of(topo: &FlankTopology, dims: &[(usize, usize); 4], stored: &[Vec<i16>; 4], k: usize) -> Vec<u8> {
    let (w, h) = dims[k];
    let mut sums = vec![0i32; w * h];
    for l in topo.links(k) {
        let (sw, sh) = (dims[l.plane as usize].0 as isize, dims[l.plane as usize].1 as isize);
        let src = &stored[l.plane as usize];
        let at = |x: isize, y: isize| src[(y.clamp(0, sh - 1) * sw + x.clamp(0, sw - 1)) as usize] as i32;
        for y in 0..h {
            for x in 0..w {
                sums[y * w + x] += at(x as isize + l.a.0 as isize, y as isize + l.a.1 as isize) + at(x as isize + l.b.0 as isize, y as isize + l.b.1 as isize);
            }
        }
    }
    sums.iter().map(|&v| flank_base(v) as u8).collect()
}

pub fn prep(path: &str) -> Prep {
    let bytes = std::fs::read(path).unwrap();
    let hdr = pkon::parse_header(&bytes).unwrap();
    let CodecParams::Ulisa(p) = &hdr.params else { panic!("not codec 7") };
    let payload = bytes[pkon::FIXED_HEADER_LEN + hdr.meta_len as usize..hdr.file_len() as usize].to_vec();
    let frame = BayerFrame::with_lattice(p.metadata.clone(), hdr.bit_depth, p.lattice).unwrap();
    let depth = u32::from(frame.coding_depth());
    let dims = frame.plane_dims();
    // Production decode (verifies the pixel CRC) gives the reference samples.
    let Decoded::Raw(img) = pkon::decode(&bytes).unwrap() else { panic!() };
    let coded = frame.normalize(&img.samples).into_owned();
    let planes = frame.split(&coded);
    let topo = frame.topology().unwrap();
    let mut stored: [Vec<i16>; 4] = Default::default();
    let mut bases = Vec::new();
    for k in 0..4 {
        let (w, h) = dims[k];
        let mut s1 = Stage1::new(w, h, depth).unwrap();
        let (mut r, mut l) = (vec![0i32; w * h], vec![0u8; w * h]);
        s1.encode_tree(&planes[k], &mut r, &mut l).unwrap();
        bases.push(bases_of(&topo, &dims, &stored, k));
        stored[k] = r.iter().map(|&v| v.clamp(i16::MIN as i32, i16::MAX as i32) as i16).collect();
    }
    let mut padded = payload.clone();
    padded.extend_from_slice(&[0u8; 8]);
    Prep {
        frame,
        topo,
        dims,
        depth,
        bit_depth: hdr.bit_depth,
        crc: hdr.pixel_crc32,
        payload,
        padded,
        bases,
        samples: hdr.width as f64 * hdr.height as f64,
        ref_samples: img.samples,
    }
}

/// The decode loop of `FlankCodec::decode_planes` minus the flank-base construction (precomputed in `Prep`, identical values).
fn drive<I: BitIo>(io: &mut I, pr: &Prep, out: &mut [Vec<u16>; 4]) {
    let mut fm = FlankModel::new();
    for (k, o) in out.iter_mut().enumerate() {
        let (w, h) = pr.dims[k];
        let mut s1 = Stage1::new(w, h, pr.depth).unwrap();
        let mut res = vec![0i32; w * h];
        let mut s3 = Stage3::with_profile(w, h, pr.depth, Stage3Profile::Bc255S11Tree).unwrap();
        s3.attach_flank(fm);
        s1.decode_tree_flank(&mut s3, io, o, &mut res, &pr.bases[k]).unwrap();
        fm = s3.detach_flank();
    }
}

pub fn new_planes(pr: &Prep) -> [Vec<u16>; 4] {
    std::array::from_fn(|k| vec![0u16; pr.dims[k].0 * pr.dims[k].1])
}

/// Decode the four planes with variant `v`; returns the number of payload bytes consumed.
pub fn decode_planes(v: V, pr: &Prep, out: &mut [Vec<u16>; 4]) -> usize {
    match v {
        V::A => {
            let mut io = Stage4Dec::new(&pr.payload);
            drive(&mut io, pr, out);
            io.consumed()
        }
        V::B => {
            let mut io = Copy0::new(&pr.payload);
            drive(&mut io, pr, out);
            io.pos
        }
        V::C => {
            let mut io = Bf(Copy0::new(&pr.payload));
            drive(&mut io, pr, out);
            io.0.pos
        }
        V::D => {
            let mut io = BfPd(Padded::new(&pr.padded));
            drive(&mut io, pr, out);
            io.0.pos
        }
        V::E => {
            let mut io = Raw0(Copy0::new(&pr.payload));
            drive(&mut io, pr, out);
            io.0.pos
        }
        V::F => {
            let mut io = BfPdRaw(Padded::new(&pr.padded));
            drive(&mut io, pr, out);
            io.0.pos
        }
        V::G => {
            let mut io = BfRaw(Copy0::new(&pr.payload));
            drive(&mut io, pr, out);
            io.0.pos
        }
    }
}

/// Merge, denormalise and compare with the production decoder's samples; returns (samples equal, CRC equal).
pub fn check(pr: &Prep, planes: &[Vec<u16>; 4]) -> (bool, bool) {
    let mut s = pr.frame.merge(planes);
    pr.frame.denormalize(&mut s).unwrap();
    (s == pr.ref_samples, pkon::pixel_crc_u16(&s, pr.bit_depth) == pr.crc)
}
