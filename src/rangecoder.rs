//! Shared binary range coder and adaptive probability bit, used by LISA.
//!
//! Extracted verbatim from the former PKON2 coder (`elliex`): the LZMA-style range coder with 16-bit probabilities
//! (`BitIo`, `BinEnc`, `BinDec`) and the count-based adaptive probability `Bit`. Probability adaptation must stay
//! bit-exact: LISA's codec 4 streams depend on it.
use std::sync::OnceLock;

const LIMIT: usize = 255;

fn rate() -> &'static [u32; LIMIT + 1] {
    static T: OnceLock<[u32; LIMIT + 1]> = OnceLock::new();
    T.get_or_init(|| {
        let mut rate = [0u32; LIMIT + 1];
        for n in 0..=LIMIT {
            rate[n] = (65536.0 / (n as f64 + 1.5)) as u32;
        }
        rate
    })
}

#[derive(Clone, Copy)]
pub(crate) struct Bit {
    p: u16, // P(bit = 1), 16-bit
    n: u8,
}
impl Bit {
    pub(crate) const NEW: Bit = Bit { p: 32768, n: 0 };
    /// P(bit = 1), 16-bit, for `BitIo::code`.
    #[inline]
    pub(crate) fn p1(&self) -> u32 {
        self.p as u32
    }
    /// Adapts this single probability state with PKON's count-based rate.
    #[inline]
    pub(crate) fn adapt(&mut self, bit: u32) {
        self.update(bit, rate());
    }
    #[inline]
    fn update(&mut self, bit: u32, rate: &[u32; LIMIT + 1]) {
        let target = (bit as i32) << 16;
        let dp = ((target - self.p as i32) as i64 * rate[self.n as usize] as i64) >> 16;
        self.p = (self.p as i64 + dp).clamp(32, 65535 - 32) as u16;
        if (self.n as usize) < LIMIT {
            self.n += 1;
        }
    }
}

// ---------- binary range coder (LZMA-style carry handling, 16-bit probabilities) ----------

pub trait BitIo {
    /// P(bit=1) as 16 bits. Encoders take `bit` and return it; decoders ignore it and return the decoded bit.
    fn code(&mut self, p1: u32, bit: u32) -> u32;
}

pub struct BinEnc {
    low: u64,
    range: u32,
    cache: u8,
    cache_size: u64,
    out: Vec<u8>,
}
impl BinEnc {
    pub fn new() -> Self {
        BinEnc { low: 0, range: u32::MAX, cache: 0, cache_size: 1, out: vec![] }
    }
    fn shift_low(&mut self) {
        if (self.low as u32) < 0xFF00_0000 || (self.low >> 32) != 0 {
            let carry = (self.low >> 32) as u8;
            let mut t = self.cache;
            loop {
                self.out.push(t.wrapping_add(carry));
                t = 0xFF;
                self.cache_size -= 1;
                if self.cache_size == 0 {
                    break;
                }
            }
            self.cache = ((self.low >> 24) & 0xFF) as u8;
        }
        self.cache_size += 1;
        self.low = (self.low & 0x00FF_FFFF) << 8;
    }
    pub fn finish(mut self) -> Vec<u8> {
        for _ in 0..5 {
            self.shift_low();
        }
        self.out.remove(0);
        self.out
    }
}
impl BitIo for BinEnc {
    #[inline]
    fn code(&mut self, p1: u32, bit: u32) -> u32 {
        let bound = ((self.range as u64 * p1 as u64) >> 16) as u32;
        if bit == 1 {
            self.range = bound;
        } else {
            self.low += bound as u64;
            self.range -= bound;
        }
        while self.range < (1 << 24) {
            self.range <<= 8;
            self.shift_low();
        }
        bit
    }
}

pub struct BinDec<'a> {
    code: u32,
    range: u32,
    buf: &'a [u8],
    pos: usize,
}
impl<'a> BinDec<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        let mut d = BinDec { code: 0, range: u32::MAX, buf, pos: 0 };
        for _ in 0..4 {
            d.code = (d.code << 8) | d.next() as u32;
        }
        d
    }
    fn next(&mut self) -> u8 {
        let b = self.buf.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        b
    }
    /// Bytes read so far. Past `buf.len()` the decoder reads implicit zeros, so `consumed() > buf.len()` means over-read.
    pub fn consumed(&self) -> usize {
        self.pos
    }
}
impl BitIo for BinDec<'_> {
    #[inline]
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
        while self.range < (1 << 24) {
            self.code = (self.code << 8) | self.next() as u32;
            self.range <<= 8;
        }
        bit
    }
}
