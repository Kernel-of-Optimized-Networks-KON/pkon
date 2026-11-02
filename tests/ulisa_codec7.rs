//! Gate for ULISA (codec 7): exact round trips through the container, frozen L1 reused from LISA, header/metadata validation,
//! the visible LISA fallback, and byte-for-byte legacy behaviour of codecs 4 and 6.
use nanoscope::canon::{Canonical, Layout, Spec};
use nanoscope::ellie::{EllieCodec, Stage3Profile, Stage4Enc, COD_P_MAX};
use nanoscope::lisa::{CfaPattern, ColorFilter, ImageMetadata, SampleLayout};
use nanoscope::pkon::{self, CodecParams, Crc32, Decoded, FIXED_HEADER_LEN, CODEC_ULISA};
use nanoscope::ulisa::{self, Engine, Fallback, Source, UlisaError};

fn lcg(seed: &mut u32) -> u32 {
    *seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
    *seed >> 8
}

fn bayer(w: usize, h: usize, e: [ColorFilter; 4]) -> ImageMetadata {
    ImageMetadata::with_cfa(w, h, SampleLayout::BayerCfa, CfaPattern { width: 2, height: 2, entries: e.to_vec(), origin_x: 0, origin_y: 0 })
}
use ColorFilter::{Blue as B, Green as G, Red as R};
const RGGB: [ColorFilter; 4] = [R, G, G, B];

/// Gradient plus noise, below `1 << bd`.
fn raw(w: usize, h: usize, bd: u8, seed: u32) -> Vec<u16> {
    let maxv = (1u32 << bd) - 1;
    let mut x = seed;
    (0..w * h)
        .map(|i| {
            let (px, py) = (i % w, i / w);
            let base = ((px * 3 + py * 5) as u32 * maxv / (3 * w + 5 * h) as u32).min(maxv);
            (base + lcg(&mut x) % (maxv / 64 + 2)).min(maxv) as u16
        })
        .collect()
}

fn roundtrip(s: &[u16], w: usize, h: usize, bd: u8, m: ImageMetadata) -> Vec<u8> {
    let f = pkon::encode_ulisa(s, w, h, bd, m.clone()).unwrap();
    assert_eq!(f[6], CODEC_ULISA);
    match pkon::decode(&f).unwrap() {
        Decoded::Raw(r) => {
            assert_eq!(r.samples, s, "{w}x{h} {bd}-bit");
            assert_eq!((r.metadata, r.bit_depth), (m, bd));
        }
        _ => panic!("ULISA must decode to a RAW image"),
    }
    f
}

#[test]
fn exact_round_trip_across_geometry_depth_and_cfa_order() {
    for (w, h) in [(64, 48), (67, 45), (2, 2), (3, 2), (2, 3), (5, 7), (130, 31)] {
        for bd in [8u8, 10, 12, 14, 16] {
            roundtrip(&raw(w, h, bd, 7 + w as u32), w, h, bd, bayer(w, h, RGGB));
        }
    }
    for e in [[B, G, G, R], [G, R, B, G], [G, B, R, G]] {
        roundtrip(&raw(40, 30, 12, 3), 40, 30, 12, bayer(40, 30, e));
    }
}

#[test]
fn extreme_content_round_trips() {
    let (w, h) = (48usize, 40usize);
    let maxv = |bd: u8| ((1u32 << bd) - 1) as u16;
    for bd in [8u8, 16] {
        roundtrip(&vec![0; w * h], w, h, bd, bayer(w, h, RGGB));
        roundtrip(&vec![maxv(bd); w * h], w, h, bd, bayer(w, h, RGGB));
        let mut x = 99;
        let noise: Vec<u16> = (0..w * h).map(|_| (lcg(&mut x) & maxv(bd) as u32) as u16).collect();
        roundtrip(&noise, w, h, bd, bayer(w, h, RGGB));
        let alt: Vec<u16> = (0..w * h).map(|i| if (i + i / w) % 2 == 0 { 0 } else { maxv(bd) }).collect();
        roundtrip(&alt, w, h, bd, bayer(w, h, RGGB));
    }
}

#[test]
fn l1_is_the_frozen_lisa_lattice() {
    let (w, h) = (60usize, 40usize);
    let base = raw(w, h, 10, 5);
    let s: Vec<u16> = base.iter().map(|&v| 816 + 16 * (v % 900)).collect(); // g = 16, origin 816 (the Parrot case)
    let f = roundtrip(&s, w, h, 14, bayer(w, h, RGGB));
    let lisa = pkon::encode_raw(&s, w, h, 14, bayer(w, h, RGGB)).unwrap();
    let (CodecParams::Ulisa(u), CodecParams::Lisa(l)) = (pkon::parse_header(&f).unwrap().params, pkon::parse_header(&lisa).unwrap().params) else { panic!() };
    assert!(u.lattice.is_some());
    assert_eq!(u.lattice, l.lattice, "ULISA must use exactly the lattice Lisa's production path detects");
    assert_eq!(f[7], 2, "lattice present -> metadata version 2");
    let plain = roundtrip(&raw(w, h, 12, 5), w, h, 12, bayer(w, h, RGGB));
    assert_eq!(plain[7], 1);
}

#[test]
fn deterministic_and_flank_stage_is_active() {
    let (w, h) = (96usize, 80usize);
    // Planes share edges (a luminance step pattern) plus independent noise: the flank sum predicts residual signs.
    let mut x = 5;
    let mut edges = vec![0i32; w * h];
    for y in 0..h {
        for xx in 0..w {
            edges[y * w + xx] = if (xx / 7 + y / 5) % 2 == 0 { 0 } else { 180 };
        }
    }
    let s: Vec<u16> = edges.iter().map(|&e| (800 + e + (lcg(&mut x) % 9) as i32) as u16).collect();
    let m = bayer(w, h, RGGB);
    let (a, info) = pkon::encode_ulisa_report(&s, w, h, 12, m.clone()).unwrap();
    assert_eq!(a, pkon::encode_ulisa(&s, w, h, 12, m).unwrap(), "encoding is deterministic");
    assert!(info.flank.samples == (w * h) as u64 && info.flank.nonzero > info.flank.samples / 4, "flank sums must be non-neutral: {:?}", info.flank);
    // The same planes through ELLIE's tree model without the flank stage must cost more.
    let planes = [0usize, 1, 2, 3].map(|k| {
        let (ox, oy) = (k & 1, k >> 1);
        (0..h / 2).flat_map(|y| (0..w / 2).map(move |xx| (xx, y))).map(|(xx, y)| s[(2 * y + oy) * w + 2 * xx + ox]).collect::<Vec<u16>>()
    });
    let mut codec = EllieCodec::with_profile(w / 2, h / 2, 12, Stage3Profile::Bc255S11Tree).unwrap();
    let mut enc = Stage4Enc::new();
    codec.encode_planes(&[&planes[0], &planes[1], &planes[2], &planes[3]], &mut enc).unwrap();
    let plain = enc.finish().len();
    let hdr = pkon::parse_header(&a).unwrap();
    assert!((hdr.payload_len as usize) < plain, "flank stage should shrink this cross-correlated image: {} vs {plain}", hdr.payload_len);
}

#[test]
fn samples_per_byte_bound_is_derived_from_the_coder_clamp() {
    // One decision costs at least -log2(COD_P_MAX / 65536) bits; the header bound (16,384/byte) must exceed the resulting maximum.
    let min_bits = -(COD_P_MAX as f64 / 65536.0).log2();
    let theoretical = 8.0 / min_bits;
    assert!(theoretical < 16_384.0 && theoretical > 11_000.0, "{theoretical}");
    // Flat images are the densest case: none may be rejected by the header check, at sizes where termination slack matters most.
    for (w, h) in [(2, 2), (50, 40), (200, 100), (330, 330), (600, 600), (1024, 1024)] {
        for v in [0u16, 1234] {
            let f = pkon::encode_ulisa(&vec![v; w * h], w, h, 12, bayer(w, h, RGGB)).unwrap();
            let hdr = pkon::parse_header(&f).unwrap();
            assert!(((w * h) as f64 / hdr.payload_len as f64) < theoretical * 1.01 + 5.0, "{w}x{h}");
            assert!(pkon::decode(&f).is_ok(), "{w}x{h}");
        }
    }
}

fn refresh_crc(f: &mut Vec<u8>) {
    let meta_len = u16::from_le_bytes([f[10], f[11]]) as usize;
    let mut c = Crc32::new();
    c.update(&f[..28]);
    c.update(&f[FIXED_HEADER_LEN..FIXED_HEADER_LEN + meta_len]);
    f[28..32].copy_from_slice(&c.finish().to_le_bytes());
}

#[test]
fn malformed_headers_and_metadata_are_rejected_not_guessed() {
    let (w, h) = (32usize, 24usize);
    let s = raw(w, h, 12, 2);
    let good = pkon::encode_ulisa(&s, w, h, 12, bayer(w, h, RGGB)).unwrap();
    let lat: Vec<u16> = s.iter().map(|&v| 100 + 4 * (v % 500)).collect();
    let good_l = pkon::encode_ulisa(&lat, w, h, 12, bayer(w, h, RGGB)).unwrap();
    assert_eq!(good_l[7], 2);
    let mutate = |base: &Vec<u8>, f: &dyn Fn(&mut Vec<u8>)| {
        let mut b = base.clone();
        f(&mut b);
        refresh_crc(&mut b);
        pkon::decode(&b)
    };
    let m = FIXED_HEADER_LEN;
    assert!(pkon::decode(&good).is_ok());
    assert!(mutate(&good, &|b| b[m] = 2).is_err(), "unknown model profile");
    assert!(mutate(&good, &|b| b[m + 1] = 2).is_err(), "unknown cross-plane context version");
    assert!(mutate(&good, &|b| b[m + 2] = 4).is_err(), "non-Bayer layout in ULISA metadata");
    assert!(mutate(&good, &|b| b[m + 3] = 4).is_err(), "CFA is not 2x2");
    assert!(mutate(&good, &|b| b[m + 3 + 8] = 9).is_err(), "unknown colour code");
    assert!(mutate(&good, &|b| b[7] = 3).is_err(), "unknown metadata version");
    assert!(mutate(&good, &|b| b[7] = 2).is_err(), "version 2 without a lattice block");
    assert!(mutate(&good_l, &|b| b[7] = 1).is_err(), "lattice block without version 2");
    assert!(mutate(&good_l, &|b| { let n = b.len(); let k = n - u16::from_le_bytes([b[20], b[21]]) as usize - 3; b[k] = 1; b[k + 1] = 0; }).is_err(), "g = 1 lattice");
    assert!(mutate(&good, &|b| b[6] = 5).is_err(), "unassigned codec id");
    assert!(mutate(&good, &|b| b[8] = 17).is_err(), "bit depth 17");
    assert!(mutate(&good, &|b| b[9] = 3).is_err(), "channels != 1");
    assert!(mutate(&good, &|b| b[5] = 1).is_err(), "flags must be zero");
    assert!(mutate(&good, &|b| b[12..16].copy_from_slice(&1u32.to_le_bytes())).is_err(), "width 1 leaves an empty parity plane");
    assert!(mutate(&good, &|b| b.truncate(b.len() - 1)).is_err(), "truncated payload");
    assert!(mutate(&good, &|b| b.push(0)).is_err(), "trailing byte");
    // A payload bit flip must surface as an error (pixel CRC), never as silently wrong pixels or a panic.
    for i in (FIXED_HEADER_LEN + 20..good.len()).step_by(3) {
        let mut b = good.clone();
        b[i] ^= 0x10;
        assert!(pkon::decode(&b).is_err(), "flip at {i}");
    }
}

fn xtrans_like(w: usize, h: usize) -> ImageMetadata {
    let entries: Vec<ColorFilter> = (0..36).map(|i| [G, R, B][i % 3]).collect();
    ImageMetadata::with_cfa(w, h, SampleLayout::OtherCfa, CfaPattern { width: 6, height: 6, entries, origin_x: 0, origin_y: 0 })
}

#[test]
fn unsupported_cfa_falls_back_to_lisa_visibly_or_fails_when_forbidden() {
    let (w, h) = (36usize, 24usize);
    let s = raw(w, h, 14, 4);
    for (m, label) in [(xtrans_like(w, h), "6x6 CFA"), (ImageMetadata::without_cfa(w, h, SampleLayout::Monochrome), "no CFA")] {
        let src = Source::Raw { samples: &s, width: w, height: h, bit_depth: 14, metadata: m.clone() };
        let r = ulisa::encode_report(Engine::Ulisa, &src).unwrap();
        assert_eq!((r.requested, r.used), (Engine::Ulisa, Engine::Lisa), "{label}");
        assert!(r.fallback.is_some() && r.ulisa.is_none(), "{label}");
        assert_eq!(r.bytes, pkon::encode_raw(&s, w, h, 14, m.clone()).unwrap(), "{label}: the fallback is LISA's own output");
        assert_eq!(ulisa::engine_of(&r.bytes).unwrap(), Engine::Lisa);
        assert_eq!(ulisa::encode(Engine::Ulisa, &src).unwrap(), r.bytes);
        assert!(matches!(ulisa::encode_with(Engine::Ulisa, &src, Fallback::None), Err(UlisaError::BayerUnsupported(_))), "{label}");
    }
    // A mosaic too small for four parity planes is also outside the frame.
    let src = Source::Raw { samples: &[1, 2], width: 1, height: 2, bit_depth: 12, metadata: bayer(1, 2, RGGB) };
    assert_eq!(ulisa::encode_report(Engine::Ulisa, &src).unwrap().used, Engine::Lisa);
    // Supported input is never diverted, and real errors are not hidden by the fallback.
    let ok = Source::Raw { samples: &s, width: w, height: h, bit_depth: 14, metadata: bayer(w, h, RGGB) };
    let r = ulisa::encode_report(Engine::Ulisa, &ok).unwrap();
    assert_eq!((r.used, r.fallback, ulisa::engine_of(&r.bytes).unwrap()), (Engine::Ulisa, None, Engine::Ulisa));
    let mut bad = s.clone();
    bad[5] = 1 << 14; // exceeds the declared depth
    let bad_src = Source::Raw { samples: &bad, width: w, height: h, bit_depth: 14, metadata: bayer(w, h, RGGB) };
    assert!(ulisa::encode_report(Engine::Ulisa, &bad_src).is_err());
    let std_img = Canonical::new(Spec { width: 4, height: 4, layout: Layout::Gray, bit_depth: 8 }, vec![7; 16]).unwrap();
    assert!(matches!(ulisa::encode(Engine::Ulisa, &Source::Standard(&std_img)), Err(UlisaError::EngineSourceMismatch { .. })));
    // The decoder entry point identifies and decodes all three engines.
    match ulisa::decode(&ulisa::encode(Engine::Ulisa, &ok).unwrap()).unwrap() {
        (Engine::Ulisa, Decoded::Raw(r)) => assert_eq!(r.samples, s),
        _ => panic!(),
    }
}

// ---------------------------------------------------------------- legacy codecs 4 and 6 are untouched

fn std_golden_img(w: u32, h: u32, layout: Layout, bd: u8, seed: u32) -> Canonical {
    let mut x = seed;
    let n = w as usize * h as usize * layout.channels();
    let data: Vec<u8> = if bd == 8 {
        (0..n).map(|i| ((i / layout.channels()) as u8).wrapping_add((lcg(&mut x) & 15) as u8)).collect()
    } else {
        (0..n)
            .flat_map(|i| {
                let v = ((i / layout.channels()) as u16).wrapping_mul(37).wrapping_add((lcg(&mut x) & 255) as u16);
                v.to_le_bytes()
            })
            .collect()
    };
    Canonical::new(Spec { width: w, height: h, layout, bit_depth: bd }, data).unwrap()
}

/// The ELLIE golden files were written by the code as it was before ULISA existed (`tests/golden_pkon`); LISA's are covered by
/// `pkon_container::golden_v2_files_decode_and_reencode_byte_exact`.
#[test]
fn ellie_codec6_output_is_byte_identical_to_the_pre_ulisa_goldens() {
    let dir = std::path::Path::new("tests/golden_pkon");
    for (name, img) in [
        ("ellie_rgb8_33x21.pkon", std_golden_img(33, 21, Layout::Rgb, 8, 11)),
        ("ellie_gray16_29x17.pkon", std_golden_img(29, 17, Layout::Gray, 16, 5)),
        ("ellie_rgba8_20x14.pkon", std_golden_img(20, 14, Layout::Rgba, 8, 3)),
    ] {
        let on_disk = std::fs::read(dir.join(name)).unwrap();
        assert_eq!(pkon::encode_standard(&img).unwrap(), on_disk, "{name}: ELLIE encoder output changed");
        match pkon::decode(&on_disk).unwrap() {
            Decoded::Standard(c) => assert_eq!(c.data, img.data, "{name}"),
            _ => panic!(),
        }
    }
}

#[test]
fn lisa_codec4_still_encodes_through_the_unchanged_path() {
    let (w, h) = (64usize, 48usize);
    let s = raw(w, h, 12, 9);
    let f = ulisa::encode(Engine::Lisa, &Source::Raw { samples: &s, width: w, height: h, bit_depth: 12, metadata: bayer(w, h, RGGB) }).unwrap();
    assert_eq!(f[6], 4);
    assert_eq!(f, pkon::encode_raw(&s, w, h, 12, bayer(w, h, RGGB)).unwrap());
    assert!(matches!(pkon::decode(&f).unwrap(), Decoded::Raw(r) if r.samples == s));
}
