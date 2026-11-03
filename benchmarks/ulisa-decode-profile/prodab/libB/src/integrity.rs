//! Integrity measurements shown by the converter UI. Pure functions over the canonical sample bytes.
//!
//! Pixel SHA-256 is taken over exactly the byte stream the `.pkon` pixel CRC-32 covers (`pkon::pixel_crc_u16` for RAW,
//! the `Canonical` data for standard images): row-major samples, one value per sample, 1 byte per sample for
//! bit depth <= 8, otherwise 2 bytes little-endian; standard images keep their interleaved channel order.
use crate::canon::Canonical;
use crate::pkon::bytes_per_sample;
use sha2::{Digest, Sha256};

pub const STANDARD_BYTE_REPR: &str = "8-bit samples, row-major, channels interleaved";
pub const RAW_BYTE_REPR_16: &str = "row-major samples, 2 bytes each, little-endian";
pub const RAW_BYTE_REPR_8: &str = "row-major samples, 1 byte each";

fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// SHA-256 of arbitrary bytes (used for the complete `.pkon` file).
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// Pixel SHA-256 of a standard image: its canonical data buffer.
pub fn pixel_sha256_standard(img: &Canonical) -> String {
    sha256_hex(&img.data)
}

/// Pixel SHA-256 of RAW samples, serialized exactly as `pkon::pixel_crc_u16` does. Streams; no full-size copy.
pub fn pixel_sha256_u16(samples: &[u16], bit_depth: u8) -> String {
    let mut h = Sha256::new();
    let wide = bytes_per_sample(bit_depth) == 2;
    let mut buf = [0u8; 4096];
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
        h.update(&buf[..n]);
    }
    hex(&h.finalize())
}

pub fn raw_byte_repr(bit_depth: u8) -> &'static str {
    if bytes_per_sample(bit_depth) == 2 { RAW_BYTE_REPR_16 } else { RAW_BYTE_REPR_8 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors_and_u16_matches_flat_bytes() {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let s = [0x0102u16, 0x0304, 0x0fff];
        assert_eq!(pixel_sha256_u16(&s, 12), sha256_hex(&[2, 1, 4, 3, 0xff, 0x0f]));
        assert_eq!(pixel_sha256_u16(&[1, 2, 255], 8), sha256_hex(&[1, 2, 255]));
        let big: Vec<u16> = (0..10_000u32).map(|i| (i * 31) as u16).collect();
        let flat: Vec<u8> = big.iter().flat_map(|s| s.to_le_bytes()).collect();
        assert_eq!(pixel_sha256_u16(&big, 16), sha256_hex(&flat));
    }
}
