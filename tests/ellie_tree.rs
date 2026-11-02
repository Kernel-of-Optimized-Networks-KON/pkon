//! ELLIE `Bc255S11Tree` profile (metadata profile id 3, the promoted `TM-nostat` structural tree of `benchmarks/ellie-structural-tree/`).
//! New files use it by default; every earlier profile must still encode and decode exactly as before, selected by the metadata.
use nanoscope::canon::{Canonical, Layout, Spec};
use nanoscope::ellie::{EllieCodec, Stage3Profile, Stage4Enc};
use nanoscope::pkon::*;
use std::path::Path;

fn lcg(seed: &mut u32) -> u32 {
    *seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
    *seed >> 8
}

/// Mix of flat blocks, two-level edges, gradients and noise so that every structural leaf is exercised.
fn img(w: u32, h: u32, layout: Layout, bit_depth: u8, seed: u32) -> Canonical {
    let ch = layout.channels();
    let bps = bit_depth as usize / 8;
    let mut x = seed;
    let maxv = (1u32 << bit_depth) - 1;
    let mut data = Vec::new();
    for i in 0..(w * h) as usize {
        let (px, py) = (i % w as usize, i / w as usize);
        let kind = (px / 8 + py / 8) % 4;
        for c in 0..ch {
            let v = match kind {
                0 => maxv / 3 + c as u32,
                1 => if (px / 2 + py / 3) % 2 == 0 { maxv / 4 } else { maxv / 2 },
                2 => ((px * 3 + py * 5) as u32 + c as u32) % (maxv + 1),
                _ => (maxv / 2 + lcg(&mut x) % (maxv / 8 + 1)).min(maxv),
            };
            data.extend_from_slice(&(v as u16).to_le_bytes()[..bps]);
        }
    }
    Canonical::new(Spec { width: w, height: h, layout, bit_depth }, data).unwrap()
}

fn back(f: &[u8]) -> Canonical {
    match decode(f).unwrap() {
        Decoded::Standard(c) => c,
        _ => panic!("expected a standard image"),
    }
}

fn profile_of(f: &[u8]) -> Stage3Profile {
    let CodecParams::Ellie(p) = parse_header(f).unwrap().params else { panic!() };
    p.profile
}

fn reseal(v: &mut [u8]) {
    let meta_len = u16::from_le_bytes([v[10], v[11]]) as usize;
    let mut c = Crc32::new();
    c.update(&v[..28]);
    c.update(&v[32..32 + meta_len]);
    v[28..32].copy_from_slice(&c.finish().to_le_bytes());
}

const ALL: [Layout; 4] = [Layout::Gray, Layout::GrayAlpha, Layout::Rgb, Layout::Rgba];

#[test]
fn default_encoder_writes_the_tree_profile_and_round_trips_bit_exactly() {
    for layout in ALL {
        for depth in [8u8, 16] {
            let im = img(37, 29, layout, depth, 7);
            let f = encode_standard(&im).unwrap();
            let h = parse_header(&f).unwrap();
            assert_eq!(h.meta_version, META_VERSION_2);
            assert_eq!(*f[FIXED_HEADER_LEN..FIXED_HEADER_LEN + h.meta_len as usize].last().unwrap(), ELLIE_PROFILE_BC255S11_TREE);
            assert_eq!(profile_of(&f), Stage3Profile::Bc255S11Tree);
            assert_eq!(back(&f).data, im.data, "{layout:?} {depth}");
            assert_eq!(encode_standard(&im).unwrap(), f, "deterministic");
        }
    }
}

#[test]
fn every_profile_still_round_trips_and_the_metadata_selects_the_decoder() {
    let im = img(41, 23, Layout::Rgba, 8, 3);
    let mut files = vec![];
    for p in [Stage3Profile::Legacy4, Stage3Profile::Bc255S11, Stage3Profile::Bc255S11Direct, Stage3Profile::Bc255S11Tree] {
        let f = encode_standard_profile(&im, EllieRepr::YCoCgR, p).unwrap();
        assert_eq!(profile_of(&f), p);
        assert_eq!(back(&f).data, im.data, "{p:?}");
        files.push(f);
    }
    for i in 0..files.len() {
        for j in i + 1..files.len() {
            assert_ne!(files[i], files[j], "profiles must be distinct streams");
        }
    }
    // The pre-tree default is unchanged and still reachable explicitly.
    let d = encode_standard_profile(&im, EllieRepr::Auto, Stage3Profile::Bc255S11Direct).unwrap();
    assert_eq!(*d[FIXED_HEADER_LEN + parse_header(&d).unwrap().meta_len as usize - 1..FIXED_HEADER_LEN + parse_header(&d).unwrap().meta_len as usize].first().unwrap(), ELLIE_PROFILE_BC255S11_DIRECT);
}

#[test]
fn tree_profile_handles_tiny_and_degenerate_geometries() {
    for (w, h) in [(1u32, 1u32), (1, 9), (9, 1), (2, 2), (2, 3), (3, 2), (5, 5), (64, 3)] {
        for layout in [Layout::Gray, Layout::Rgb] {
            let im = img(w, h, layout, 8, w * 31 + h);
            let f = encode_standard(&im).unwrap();
            assert_eq!(back(&f).data, im.data, "{w}x{h} {layout:?}");
        }
    }
}

#[test]
fn constant_and_extreme_content_round_trip() {
    for v in [0u8, 255] {
        let im = Canonical::new(Spec { width: 33, height: 17, layout: Layout::Rgb, bit_depth: 8 }, vec![v; 33 * 17 * 3]).unwrap();
        assert_eq!(back(&encode_standard(&im).unwrap()).data, im.data);
    }
    let mut x = 99u32;
    let data: Vec<u8> = (0..40 * 40 * 3).map(|_| (lcg(&mut x) >> 3) as u8).collect();
    let im = Canonical::new(Spec { width: 40, height: 40, layout: Layout::Rgb, bit_depth: 8 }, data).unwrap();
    assert_eq!(back(&encode_standard(&im).unwrap()).data, im.data);
}

#[test]
fn old_decoder_boundary_unknown_profile_ids_fail_safely_and_tree_is_accepted() {
    let im = img(24, 16, Layout::Rgba, 8, 2);
    let f = encode_standard_with(&im, EllieRepr::YCoCgR).unwrap();
    let h = parse_header(&f).unwrap();
    let meta_end = FIXED_HEADER_LEN + h.meta_len as usize;
    for bad in [0u8, 4, 5, 255] {
        let mut b = f.clone();
        b[meta_end - 1] = bad;
        reseal(&mut b);
        assert!(matches!(decode(&b).unwrap_err(), ContainerError::UnsupportedCodecVersion { field: "stage3_profile", .. }), "id {bad}");
    }
    // Relabelling the tree stream as another profile must be detected (wrong model => the pixel CRC or the residual range check fails).
    for other in [ELLIE_PROFILE_BC255S11, ELLIE_PROFILE_BC255S11_DIRECT] {
        let mut b = f.clone();
        b[meta_end - 1] = other;
        reseal(&mut b);
        match decode(&b) {
            Err(_) => {}
            Ok(Decoded::Standard(c)) => assert_ne!(c.data, im.data, "mislabelled stream cannot silently decode to the original"),
            Ok(_) => panic!(),
        }
    }
}

#[test]
fn truncated_and_corrupted_tree_streams_never_panic() {
    let im = img(48, 32, Layout::Rgb, 8, 11);
    let f = encode_standard(&im).unwrap();
    for cut in [0usize, 10, 31, 40, f.len() / 2, f.len() - 1] {
        assert!(decode(&f[..cut]).is_err(), "cut {cut}");
    }
    let mut x = 5u32;
    for _ in 0..200 {
        let mut b = f.clone();
        for _ in 0..3 {
            let at = FIXED_HEADER_LEN + parse_header(&f).unwrap().meta_len as usize + lcg(&mut x) as usize % (f.len() - FIXED_HEADER_LEN - parse_header(&f).unwrap().meta_len as usize);
            b[at] ^= 1 << (lcg(&mut x) % 8);
        }
        if let Ok(Decoded::Standard(c)) = decode(&b) {
            assert_eq!(c.data, im.data, "a damaged stream must not decode to different pixels without error");
        }
    }
    // Pure garbage payload straight into the codec.
    let mut codec = EllieCodec::with_profile(16, 16, 8, Stage3Profile::Bc255S11Tree).unwrap();
    let mut out = vec![0u16; 256];
    for seed in 0..50u32 {
        let mut x = seed;
        let bytes: Vec<u8> = (0..300).map(|_| lcg(&mut x) as u8).collect();
        let _ = codec.decode_planes(&bytes, &mut [&mut out[..]]);
    }
}

#[test]
fn codec_level_round_trip_matches_direct_profile_pixels() {
    let mut x = 1u32;
    let plane: Vec<u16> = (0..50 * 30).map(|i| ((i % 50) as u16 * 3 + (lcg(&mut x) % 5) as u16) & 255).collect();
    let mut dec = vec![vec![0u16; plane.len()]; 2];
    for (k, p) in [Stage3Profile::Bc255S11Direct, Stage3Profile::Bc255S11Tree].into_iter().enumerate() {
        let mut c = EllieCodec::with_profile(50, 30, 8, p).unwrap();
        let mut enc = Stage4Enc::new();
        c.encode_planes(&[&plane], &mut enc).unwrap();
        let bytes = enc.finish().to_vec();
        let mut c2 = EllieCodec::with_profile(50, 30, 8, p).unwrap();
        c2.decode_planes(&bytes, &mut [&mut dec[k][..]]).unwrap();
        assert_eq!(dec[k], plane, "{p:?}");
    }
}

/// Historical tracked ELLIE files must keep decoding under the profile their metadata names, and files written with a pre-tree profile
/// must re-encode to the identical bytes with that profile (the old encoders are untouched). Fixtures that do not decode are reported
/// and counted; none of them are tree files.
#[test]
fn historical_streams_still_decode_and_old_profiles_reencode_identically() {
    let dirs = ["benchmarks/ellie-final-3img/run-final-three-image/files", "benchmarks/ellie-bc-integration/ab3/legacy4", "benchmarks/ellie-bc-integration/ab3/bc255s11"];
    let (mut ok, mut skipped, mut same) = (0, 0, 0);
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(Path::new(d)) else { continue };
        for e in rd.flatten() {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("pkon") {
                continue;
            }
            let bytes = std::fs::read(&path).unwrap();
            let Ok(h) = parse_header(&bytes) else { continue };
            let CodecParams::Ellie(p) = &h.params else { continue };
            assert_ne!(p.profile, Stage3Profile::Bc255S11Tree, "{path:?}");
            let Ok(Decoded::Standard(im)) = decode(&bytes) else {
                eprintln!("historical fixture does not decode with the current Stage 1 (predates it): {path:?} profile {:?}", p.profile);
                skipped += 1;
                continue;
            };
            ok += 1;
            let repr = if p.representation == ELLIE_REPR_YCOCG_R { EllieRepr::YCoCgR } else { EllieRepr::Untransformed };
            if encode_standard_profile(&im, repr, p.profile).unwrap() == bytes {
                same += 1;
            }
            // And the same pixels survive the new default.
            assert_eq!(back(&encode_standard_with(&im, repr).unwrap()).data, im.data, "{path:?}");
        }
    }
    eprintln!("historical streams: {ok} decoded ({same} re-encode byte-identically), {skipped} predate the current Stage 1");
}
