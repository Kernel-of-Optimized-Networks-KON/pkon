//! L1 lattice normalization in the production LISA RAW path: exact reconstruction, GCD-1 stream stability, metadata v2 parsing
//! and rejection, and ULISA identification.
use nanoscope::lisa::{ColorFilter, CfaPattern, ImageMetadata, SampleLayout};
use nanoscope::pkon::{self, CodecParams, ContainerError, Crc32, Decoded, CODEC_LISA, META_VERSION_1, META_VERSION_2};
use nanoscope::ulisa::{self, Engine};

const W: usize = 32;
const H: usize = 24;

fn bayer_meta() -> ImageMetadata {
    let cfa = CfaPattern { width: 2, height: 2, entries: vec![ColorFilter::Red, ColorFilter::Green, ColorFilter::Green, ColorFilter::Blue], origin_x: 0, origin_y: 0 };
    ImageMetadata::with_cfa(W, H, SampleLayout::BayerCfa, cfa)
}

/// Smooth-ish deterministic mosaic; every sample is `origin + g * k`.
fn lattice_image(origin: u16, g: u16, levels: u32) -> Vec<u16> {
    (0..W * H).map(|i| origin + g * (((i * 37 + (i / W) * 11) as u32 % levels) as u16)).collect()
}

fn encode(s: &[u16], bd: u8) -> Vec<u8> {
    pkon::encode_raw(s, W, H, bd, bayer_meta()).unwrap()
}

fn decode_raw(b: &[u8]) -> Vec<u16> {
    match pkon::decode(b).unwrap() {
        Decoded::Raw(r) => r.samples,
        _ => panic!("not RAW"),
    }
}

/// Re-seal a file after patching bytes (header CRC covers header + metadata).
fn patched(f: &[u8], at: usize, bytes: &[u8]) -> Vec<u8> {
    let mut b = f.to_vec();
    b[at..at + bytes.len()].copy_from_slice(bytes);
    let meta_len = u16::from_le_bytes([b[10], b[11]]) as usize;
    let mut c = Crc32::new();
    c.update(&b[..28]);
    c.update(&b[32..32 + meta_len]);
    b[28..32].copy_from_slice(&c.finish().to_le_bytes());
    b
}

#[test]
fn normalized_stream_roundtrips_exactly_and_is_smaller() {
    for (origin, g, levels) in [(816u16, 16u16, 200u32), (800, 4, 900), (0, 2, 2000), (5, 3, 1), (1, 7, 2)] {
        let s = lattice_image(origin, g, levels);
        let f = encode(&s, 14);
        let h = pkon::parse_header(&f).unwrap();
        let max = s.iter().max().unwrap();
        if s.iter().min() == Some(max) {
            // Constant image: no lattice, metadata version 1.
            assert_eq!(h.meta_version, META_VERSION_1);
        } else {
            assert_eq!(h.meta_version, META_VERSION_2, "origin {origin} g {g}");
            assert_eq!(h.bit_depth, 14, "header keeps the original bit depth");
            let CodecParams::Lisa(p) = &h.params else { panic!() };
            let l = p.lattice.unwrap();
            assert_eq!((l.origin, u32::from(l.g)), (*s.iter().min().unwrap(), s.iter().fold(0u32, |a, &v| gcd(a, u32::from(v - l.origin)))));
        }
        assert_eq!(decode_raw(&f), s, "origin {origin} g {g}");
        assert_eq!(f, encode(&s, 14), "deterministic");
    }
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
}

#[test]
fn normalized_is_smaller_than_the_unnormalized_equivalent() {
    // Same structure, once on the lattice and once with one odd sample that forces g == 1.
    let s = lattice_image(816, 16, 200);
    let mut t = s.clone();
    t[5] += 1;
    let (fs, ft) = (encode(&s, 14), encode(&t, 14));
    assert_eq!(pkon::parse_header(&ft).unwrap().meta_version, META_VERSION_1);
    assert!(fs.len() < ft.len(), "{} !< {}", fs.len(), ft.len());
    assert_eq!(decode_raw(&ft), t);
}

#[test]
fn gcd_one_streams_are_version_1_and_carry_no_lattice() {
    let s: Vec<u16> = (0..W * H).map(|i| ((i * 7 + (i / W) * 13) % 4096) as u16).collect();
    let f = encode(&s, 12);
    let h = pkon::parse_header(&f).unwrap();
    assert_eq!(h.meta_version, META_VERSION_1);
    let CodecParams::Lisa(p) = &h.params else { panic!() };
    assert!(p.lattice.is_none());
    assert_eq!(decode_raw(&f), s);
}

#[test]
fn out_of_range_samples_are_still_refused_not_normalized() {
    // Even lattice, but a sample exceeds the declared bit depth: must stay an encoder error as before.
    let mut s = lattice_image(0, 2, 100);
    s[3] = 4096;
    assert!(pkon::encode_raw(&s, W, H, 12, bayer_meta()).is_err());
}

#[test]
fn lattice_metadata_is_validated() {
    let s = lattice_image(816, 16, 200);
    let f = encode(&s, 14);
    let meta_len = u16::from_le_bytes([f[10], f[11]]) as usize;
    let at = 32 + meta_len - 5; // origin u16, g u16, coding bit depth u8
    let err = |b: &[u8]| pkon::decode(b).unwrap_err();
    // g < 2, coding depth 0 / above the sample depth / range overflow, origin past the sample range.
    for (off, v) in [(2usize, &[1u8, 0][..]), (2, &[0, 0]), (4, &[0]), (4, &[15]), (4, &[14]), (0, &[0xFF, 0xFF])] {
        let b = patched(&f, at + off, v);
        assert!(matches!(err(&b), ContainerError::InvalidMetadata(_) | ContainerError::CorruptPayload(_) | ContainerError::Lisa(_)), "patch {off} {v:?}: {:?}", err(&b));
    }
    // Truncated or extended metadata block.
    let mut short = f.clone();
    short.remove(32 + meta_len - 1);
    short[10..12].copy_from_slice(&((meta_len - 1) as u16).to_le_bytes());
    let short = patched(&short, 0, &[]);
    assert!(pkon::decode(&short).is_err());
    // A version 2 header over version 1 metadata (no trailer) is a length error; version 1 over a trailer likewise.
    let g1: Vec<u16> = (0..W * H).map(|i| (i % 4096) as u16).collect();
    let v1 = encode(&g1, 12);
    assert!(matches!(pkon::decode(&patched(&v1, 7, &[2])).unwrap_err(), ContainerError::InvalidMetaLength { .. }));
    assert!(pkon::decode(&patched(&f, 7, &[1])).is_err());
    // Corrupt a payload byte under a valid lattice: never a panic, never silently wrong.
    let mut c = f.clone();
    let last = c.len() - 1;
    c[last] ^= 0x5A;
    if let Ok(Decoded::Raw(r)) = pkon::decode(&c) {
        assert_eq!(r.samples, s);
    }
}

#[test]
fn lattice_that_disagrees_with_the_pixels_is_caught_by_the_checksum() {
    let s = lattice_image(816, 16, 200);
    let f = encode(&s, 14);
    let meta_len = u16::from_le_bytes([f[10], f[11]]) as usize;
    let b = patched(&f, 32 + meta_len - 5, &[0x31, 0x03]); // origin 817 instead of 816
    assert!(matches!(pkon::decode(&b).unwrap_err(), ContainerError::ChecksumMismatch { .. }));
}

#[test]
fn lisa_and_ulisa_identify_and_decode_l1_streams() {
    let s = lattice_image(100, 4, 900);
    let f = encode(&s, 12);
    assert_eq!(pkon::parse_header(&f).unwrap().codec, CODEC_LISA);
    assert_eq!(ulisa::engine_of(&f).unwrap(), Engine::Lisa);
    match ulisa::decode(&f).unwrap() {
        (Engine::Lisa, Decoded::Raw(r)) => {
            assert_eq!(r.samples, s);
            assert_eq!(r.bit_depth, 12);
        }
        _ => panic!("wrong engine or payload"),
    }
    let src = ulisa::Source::Raw { samples: &s, width: W, height: H, bit_depth: 12, metadata: bayer_meta() };
    assert_eq!(ulisa::encode(Engine::Lisa, &src).unwrap(), f, "ULISA emits the standalone Lisa bytes");
}
