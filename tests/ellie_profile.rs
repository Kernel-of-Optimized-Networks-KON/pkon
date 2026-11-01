//! ELLIE Stage 3 profiles in the `.pkon` container: new files name `Bc255S11` (metadata version 2), version 1 files keep
//! decoding as `Legacy4`, and damaged profile metadata fails safely.
use nanoscope::canon::{Canonical, Layout, Spec};
use nanoscope::ellie::Stage3Profile;
use nanoscope::pkon::*;

fn lcg(seed: &mut u32) -> u32 {
    *seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
    *seed >> 8
}

fn img(w: u32, h: u32, layout: Layout, bit_depth: u8, seed: u32) -> Canonical {
    let ch = layout.channels();
    let bps = bit_depth as usize / 8;
    let mut x = seed;
    let maxv = (1u32 << bit_depth) - 1;
    let mut data = Vec::new();
    for i in 0..(w * h) as usize {
        let base = ((i % w as usize) * 3 + (i / w as usize) * 5) as u32 % (maxv + 1);
        for c in 0..ch {
            let v = (base + c as u32 * 2 + lcg(&mut x) % 4).min(maxv);
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

fn reseal(v: &mut [u8]) {
    let meta_len = u16::from_le_bytes([v[10], v[11]]) as usize;
    let mut c = Crc32::new();
    c.update(&v[..28]);
    c.update(&v[32..32 + meta_len]);
    v[28..32].copy_from_slice(&c.finish().to_le_bytes());
}

fn profile_of(f: &[u8]) -> Stage3Profile {
    let CodecParams::Ellie(p) = parse_header(f).unwrap().params else { panic!() };
    p.profile
}

const ALL: [Layout; 4] = [Layout::Gray, Layout::GrayAlpha, Layout::Rgb, Layout::Rgba];

#[test]
fn new_files_name_the_bc_profile_in_version_2_metadata() {
    for layout in ALL {
        let im = img(19, 13, layout, 8, 3);
        let f = encode_standard(&im).unwrap();
        let h = parse_header(&f).unwrap();
        assert_eq!(h.meta_version, META_VERSION_2);
        assert_eq!(*f[FIXED_HEADER_LEN..FIXED_HEADER_LEN + h.meta_len as usize].last().unwrap(), ELLIE_PROFILE_BC255S11);
        assert_eq!(profile_of(&f), Stage3Profile::Bc255S11);
        let legacy = encode_standard_profile(&im, EllieRepr::Auto, Stage3Profile::Legacy4).unwrap();
        assert_eq!(&f[..4], &legacy[..4], "magic unchanged");
        assert_eq!(f[4], legacy[4], "container version unchanged");
    }
}

#[test]
fn legacy_profile_writes_version_1_metadata_and_decodes_as_legacy() {
    for layout in ALL {
        for depth in [8u8, 16] {
            let im = img(21, 11, layout, depth, 5);
            for repr in [EllieRepr::Auto, EllieRepr::Untransformed] {
                let f = encode_standard_profile(&im, repr, Stage3Profile::Legacy4).unwrap();
                assert_eq!(parse_header(&f).unwrap().meta_version, META_VERSION_1);
                assert_eq!(profile_of(&f), Stage3Profile::Legacy4);
                assert_eq!(back(&f).data, im.data, "{layout:?} {depth}-bit legacy");
            }
        }
    }
}

#[test]
fn both_profiles_round_trip_every_layout_and_depth_and_are_deterministic() {
    for profile in [Stage3Profile::Legacy4, Stage3Profile::Bc255S11] {
        for layout in ALL {
            for depth in [8u8, 16] {
                let im = img(33, 17, layout, depth, 11);
                let rgb8 = depth == 8 && layout.channels() >= 3;
                let mut reprs = vec![EllieRepr::Auto, EllieRepr::Untransformed];
                if rgb8 {
                    reprs.push(EllieRepr::YCoCgR);
                }
                for repr in reprs {
                    let f = encode_standard_profile(&im, repr, profile).unwrap();
                    let d = back(&f);
                    assert!(d.data == im.data && d.spec == im.spec, "{profile:?} {layout:?} {depth} {repr:?}");
                    assert_eq!(f, encode_standard_profile(&im, repr, profile).unwrap(), "deterministic");
                }
            }
        }
    }
}

#[test]
fn the_profiles_produce_different_streams_and_decoding_follows_the_metadata() {
    let im = img(64, 48, Layout::Rgb, 8, 7);
    let a = encode_standard_profile(&im, EllieRepr::YCoCgR, Stage3Profile::Legacy4).unwrap();
    let b = encode_standard_profile(&im, EllieRepr::YCoCgR, Stage3Profile::Bc255S11).unwrap();
    assert_ne!(a, b);
    // Relabelling a new-profile payload as version 1 (legacy) must not silently decode: the length check rejects it.
    let mut c = b.clone();
    c[7] = META_VERSION_1;
    reseal(&mut c);
    assert!(decode(&c).is_err());
}

/// Files written by the previous release (version 1 metadata, original four-context model) must still decode, and
/// re-encoding the decoded image with `Legacy4` must reproduce them byte for byte. Skipped if the artifacts are absent.
#[test]
fn files_written_before_profiles_still_decode_and_reencode_identically() {
    let dir = std::path::Path::new("benchmarks/ellie-paeth/20261101-044317Z-prod-3img/files-paeth");
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some("pkon") {
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        let h = parse_header(&bytes).unwrap();
        if h.codec != CODEC_ELLIE {
            continue;
        }
        let CodecParams::Ellie(p) = &h.params else { panic!() };
        assert_eq!((h.meta_version, p.profile), (META_VERSION_1, Stage3Profile::Legacy4), "{path:?}");
        let im = back(&bytes); // the pixel CRC is verified inside decode
        let repr = if p.representation == ELLIE_REPR_YCOCG_R { EllieRepr::YCoCgR } else { EllieRepr::Untransformed };
        assert_eq!(encode_standard_profile(&im, repr, Stage3Profile::Legacy4).unwrap(), bytes, "{path:?}");
    }
}

#[test]
fn malformed_profile_metadata_and_truncation_fail_safely() {
    let im = img(24, 16, Layout::Rgba, 8, 2);
    let f = encode_standard_with(&im, EllieRepr::YCoCgR).unwrap();
    let h = parse_header(&f).unwrap();
    let meta_end = FIXED_HEADER_LEN + h.meta_len as usize;
    // Unknown profile ids.
    for bad in [0u8, 2, 3, 255] {
        let mut b = f.clone();
        b[meta_end - 1] = bad;
        reseal(&mut b);
        assert!(matches!(decode(&b).unwrap_err(), ContainerError::UnsupportedCodecVersion { field: "stage3_profile", .. }), "id {bad}");
    }
    // Unknown metadata versions.
    for v in [0u8, 3, 255] {
        let mut b = f.clone();
        b[7] = v;
        reseal(&mut b);
        assert!(matches!(decode(&b).unwrap_err(), ContainerError::UnsupportedMetaVersion { codec: CODEC_ELLIE, .. }));
    }
    // Version 2 without the profile byte, and version 1 with a stray one: wrong length either way.
    let mut no_byte = f[..meta_end - 1].to_vec();
    no_byte.extend_from_slice(&f[meta_end..]);
    no_byte[10..12].copy_from_slice(&(h.meta_len - 1).to_le_bytes());
    reseal(&mut no_byte);
    assert!(matches!(decode(&no_byte).unwrap_err(), ContainerError::InvalidMetaLength { .. }));
    let mut v1_extra = f.clone();
    v1_extra[7] = META_VERSION_1;
    reseal(&mut v1_extra);
    assert!(matches!(decode(&v1_extra).unwrap_err(), ContainerError::InvalidMetaLength { .. }));
    // Truncation at every cut point, and trailing bytes.
    for cut in 0..f.len() {
        assert!(decode(&f[..cut]).is_err(), "cut {cut}");
    }
    let mut t = f.clone();
    t.push(0);
    assert_eq!(decode(&t).unwrap_err(), ContainerError::TrailingBytes(1));
    // Payload damage never panics and never yields wrong pixels undetected.
    let mut x = 9u32;
    for _ in 0..200 {
        let mut b = f.clone();
        let at = meta_end + lcg(&mut x) as usize % (f.len() - meta_end);
        b[at] ^= 1 << (lcg(&mut x) % 8);
        match decode(&b) {
            Err(_) => {}
            Ok(Decoded::Standard(c)) => assert!(c.data == im.data && c.spec == im.spec),
            Ok(_) => panic!("codec changed"),
        }
    }
}
