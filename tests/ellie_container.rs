//! ELLIE (codec 6) in the shared `.pkon` container: representation selection, exact round trips for every layout and
//! depth, metadata survival, deterministic output, and safe failure on damaged or truncated per-plane streams.
use nanoscope::canon::{Canonical, Layout, Spec};
use nanoscope::pkon::*;

fn lcg(seed: &mut u32) -> u32 {
    *seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
    *seed >> 8
}

/// Correlated colour gradient with noise (YCoCg-R should win) or independent noise per channel (it may not).
fn img(w: u32, h: u32, layout: Layout, bit_depth: u8, correlated: bool, seed: u32) -> Canonical {
    let ch = layout.channels();
    let bps = bit_depth as usize / 8;
    let mut x = seed;
    let mut data = Vec::with_capacity(w as usize * h as usize * ch * bps);
    let maxv = (1u32 << bit_depth) - 1;
    for i in 0..(w * h) as usize {
        let base = ((i % w as usize) * 3 + (i / w as usize) * 5) as u32 % (maxv + 1);
        for c in 0..ch {
            let v = if correlated { (base + c as u32 * 2 + lcg(&mut x) % 4).min(maxv) } else { lcg(&mut x) & maxv };
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

fn repr_of(f: &[u8]) -> u8 {
    f[FIXED_HEADER_LEN + 1]
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
fn every_layout_and_depth_round_trips_in_every_valid_representation() {
    for layout in ALL {
        for bd in [8u8, 16] {
            for correlated in [true, false] {
                let im = img(13, 9, layout, bd, correlated, 7);
                for mode in [EllieRepr::Auto, EllieRepr::Untransformed] {
                    let f = encode_standard_with(&im, mode).unwrap();
                    let b = back(&f);
                    assert_eq!((b.spec, &b.data), (im.spec, &im.data), "{layout:?} {bd} {mode:?}");
                    assert_eq!(f, encode_standard_with(&im, mode).unwrap(), "deterministic");
                }
                let rgb8 = bd == 8 && matches!(layout, Layout::Rgb | Layout::Rgba);
                match encode_standard_with(&im, EllieRepr::YCoCgR) {
                    Ok(f) => {
                        assert!(rgb8);
                        assert_eq!(repr_of(&f), ELLIE_REPR_YCOCG_R);
                        assert_eq!(back(&f).data, im.data);
                    }
                    Err(e) => {
                        assert!(!rgb8);
                        assert!(matches!(e, ContainerError::UnsupportedImage(_)));
                    }
                }
            }
        }
    }
}

#[test]
fn auto_picks_the_smaller_complete_file_and_never_transforms_non_rgb() {
    let corr = img(64, 64, Layout::Rgb, 8, true, 3);
    let (p, y, a) = (
        encode_standard_with(&corr, EllieRepr::Untransformed).unwrap(),
        encode_standard_with(&corr, EllieRepr::YCoCgR).unwrap(),
        encode_standard(&corr).unwrap(),
    );
    assert_eq!(a.len(), p.len().min(y.len()));
    assert_eq!(repr_of(&a), if y.len() < p.len() { ELLIE_REPR_YCOCG_R } else { ELLIE_REPR_UNTRANSFORMED });
    assert!(y.len() < p.len(), "correlated colour should favour YCoCg-R");
    for noisy in [img(64, 64, Layout::Rgba, 8, false, 5), img(64, 64, Layout::Rgb, 8, false, 6)] {
        let (p, y, a) = (
            encode_standard_with(&noisy, EllieRepr::Untransformed).unwrap(),
            encode_standard_with(&noisy, EllieRepr::YCoCgR).unwrap(),
            encode_standard(&noisy).unwrap(),
        );
        assert_eq!(a.len(), p.len().min(y.len()), "auto never loses to either representation");
    }
    for layout in [Layout::Gray, Layout::GrayAlpha] {
        assert_eq!(repr_of(&encode_standard(&img(16, 16, layout, 8, true, 1)).unwrap()), ELLIE_REPR_UNTRANSFORMED);
    }
}

#[test]
fn tiny_and_degenerate_geometries() {
    for (w, h) in [(1, 1), (1, 7), (9, 1), (2, 2), (3, 5)] {
        for layout in ALL {
            let im = img(w, h, layout, 8, true, 11);
            assert_eq!(back(&encode_standard(&im).unwrap()).data, im.data, "{w}x{h} {layout:?}");
        }
    }
}

#[test]
fn extreme_colours_round_trip_through_the_transform() {
    // Saturated corners drive Co and Cg to +/-255.
    let px: [[u8; 3]; 8] = [[0, 0, 0], [255, 255, 255], [255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 0, 255], [0, 255, 255], [255, 255, 0]];
    let data: Vec<u8> = (0..64).flat_map(|i| px[i % 8]).collect();
    let im = Canonical::new(Spec { width: 8, height: 8, layout: Layout::Rgb, bit_depth: 8 }, data).unwrap();
    assert_eq!(back(&encode_standard_with(&im, EllieRepr::YCoCgR).unwrap()).data, im.data);
}

#[test]
fn metadata_names_representation_and_plane_layout() {
    let im = img(20, 11, Layout::Rgba, 8, true, 2);
    let f = encode_standard_with(&im, EllieRepr::YCoCgR).unwrap();
    let h = parse_header(&f).unwrap();
    assert_eq!((h.codec, h.bit_depth, h.channels, h.width, h.height), (CODEC_ELLIE, 8, 4, 20, 11));
    let CodecParams::Ellie(p) = h.params else { panic!() };
    assert_eq!(p.representation, ELLIE_REPR_YCOCG_R);
    assert_eq!(p.planes.iter().map(|x| x.0).collect::<Vec<_>>(), [8, 9, 9, 8]);
    assert_eq!(p.planes.iter().map(|x| x.1 as usize).sum::<usize>(), h.payload_len as usize);
    assert_eq!(f.len(), FIXED_HEADER_LEN + h.meta_len as usize + h.payload_len as usize);
}

#[test]
fn bad_representation_and_inconsistent_metadata_are_rejected() {
    let im = img(16, 16, Layout::Rgb, 8, true, 4);
    let f = encode_standard_with(&im, EllieRepr::YCoCgR).unwrap();
    let patch = |at: usize, v: u8| {
        let mut b = f.clone();
        b[at] = v;
        reseal(&mut b);
        decode(&b).unwrap_err()
    };
    assert!(matches!(patch(FIXED_HEADER_LEN + 1, 2), ContainerError::UnsupportedCodecVersion { codec: CODEC_ELLIE, field: "representation", value: 2 }));
    assert!(matches!(patch(FIXED_HEADER_LEN, 9), ContainerError::UnsupportedCodecVersion { .. }));
    assert!(matches!(patch(FIXED_HEADER_LEN, 0), ContainerError::InvalidMetadata(_)), "layout vs channels");
    assert!(matches!(patch(FIXED_HEADER_LEN + 2, 9), ContainerError::InvalidMetadata(_)), "plane bit depth vs representation");
    assert!(matches!(patch(FIXED_HEADER_LEN + 7, 8), ContainerError::InvalidMetadata(_)), "plane 1 must be 9-bit");
    assert!(matches!(patch(9, 5), ContainerError::InvalidChannels { .. }));
    assert!(matches!(patch(8, 12), ContainerError::UnsupportedBitDepth { .. }));
    // Stream lengths no longer summing to the payload length.
    assert!(matches!(patch(FIXED_HEADER_LEN + 3, f[FIXED_HEADER_LEN + 3].wrapping_add(1)), ContainerError::InvalidMetadata(_)));
    // YCoCg-R declared on gray is meaningless.
    let g = encode_standard(&img(16, 16, Layout::Gray, 8, true, 1)).unwrap();
    let mut b = g.clone();
    b[FIXED_HEADER_LEN + 1] = ELLIE_REPR_YCOCG_R;
    reseal(&mut b);
    assert!(decode(&b).is_err());
}

#[test]
fn damaged_and_truncated_files_fail_safely() {
    for repr in [EllieRepr::Untransformed, EllieRepr::YCoCgR] {
        let im = img(40, 30, Layout::Rgba, 8, true, 9);
        let f = encode_standard_with(&im, repr).unwrap();
        let h = parse_header(&f).unwrap();
        let payload_at = FIXED_HEADER_LEN + h.meta_len as usize;
        // Truncation at every cut point.
        for cut in (0..f.len()).step_by(7).chain([f.len() - 1]) {
            assert!(decode(&f[..cut]).is_err(), "truncated at {cut}");
        }
        // Trailing bytes.
        let mut t = f.clone();
        t.push(0);
        assert_eq!(decode(&t).unwrap_err(), ContainerError::TrailingBytes(1));
        // A flipped payload bit anywhere (header checksum still valid) is caught by the pixel checksum or stream decode.
        let mut x = 5u32;
        for _ in 0..200 {
            let mut b = f.clone();
            let at = payload_at + lcg(&mut x) as usize % (f.len() - payload_at);
            b[at] ^= 1 << (lcg(&mut x) % 8);
            assert!(decode(&b).is_err(), "bit flip at {at}");
        }
        // Whole-plane damage: zero or garble each plane's stream in turn.
        let CodecParams::Ellie(p) = h.params else { panic!() };
        let mut off = payload_at;
        for &(_, len) in &p.planes {
            for fill in [0x00u8, 0xFF] {
                let mut b = f.clone();
                b[off..off + len as usize].fill(fill);
                assert!(decode(&b).is_err());
            }
            off += len as usize;
        }
    }
}

#[test]
fn flat_images_stay_small_and_valid() {
    let im = Canonical::new(Spec { width: 512, height: 512, layout: Layout::Rgb, bit_depth: 8 }, vec![77; 512 * 512 * 3]).unwrap();
    let f = encode_standard(&im).unwrap();
    assert!(f.len() < 2000);
    assert_eq!(back(&f).data, im.data);
}

#[test]
fn decode_limit_applies_to_ellie() {
    let f = encode_standard(&img(32, 32, Layout::Rgb, 8, true, 1)).unwrap();
    assert!(matches!(decode_with_limit(&f, 100), Err(ContainerError::TooLarge { .. })));
}

#[test]
fn unassigned_and_retired_ids_stay_unassigned() {
    let f = encode_standard(&img(8, 8, Layout::Rgb, 8, true, 1)).unwrap();
    for c in [0u8, 1, 2, 3, 5, 7, 99, 255] {
        let mut b = f.clone();
        b[6] = c;
        reseal(&mut b);
        assert_eq!(decode(&b).unwrap_err(), ContainerError::UnsupportedCodec(c));
    }
}
