//! Focused gate for the shared `.pkon` container (version 2): LISA round-trips exactly, retired PKON2 codecs and v1 files are
//! rejected explicitly, versions/codecs/features are rejected explicitly, and malformed input never panics.
use nanoscope::canon::{Canonical, Layout, Spec};
use nanoscope::lisa::{CfaPattern, ColorFilter, ImageMetadata, SampleLayout};
use nanoscope::lisa_bayer_plane::Stage1Mode;
use nanoscope::pkon::*;
use nanoscope::pkonfile;

// ---------------------------------------------------------------- fixtures

fn lcg(seed: &mut u32) -> u32 {
    *seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
    *seed >> 8
}

fn std_img(w: u32, h: u32, layout: Layout, seed: u32) -> Canonical {
    let mut x = seed;
    let n = w as usize * h as usize * layout.channels();
    let data = (0..n).map(|i| ((i / layout.channels()) as u8).wrapping_add((lcg(&mut x) & 15) as u8)).collect();
    Canonical::new(Spec { width: w, height: h, layout, bit_depth: 8 }, data).unwrap()
}

/// Smooth gradient plus noise, always below `1 << bd`.
fn raw_samples(w: usize, h: usize, bd: u8, seed: u32) -> Vec<u16> {
    let maxv = (1u32 << bd) - 1;
    let mut x = seed;
    (0..w * h)
        .map(|i| {
            let (px, py) = (i % w, i / w);
            let base = ((px * 3 + py * 5) as u32 * maxv / (3 * w + 5 * h) as u32).min(maxv);
            let noise = lcg(&mut x) % (maxv / 64 + 2);
            (base + noise).min(maxv) as u16
        })
        .collect()
}

fn bayer_meta(w: usize, h: usize, entries: [ColorFilter; 4]) -> ImageMetadata {
    let cfa = CfaPattern { width: 2, height: 2, entries: entries.to_vec(), origin_x: 0, origin_y: 0 };
    ImageMetadata::with_cfa(w, h, SampleLayout::BayerCfa, cfa)
}

fn xtrans_like(w: usize, h: usize) -> ImageMetadata {
    use ColorFilter::*;
    let e = [Green, Red, Green, Green, Blue, Green, Blue, Green, Blue, Red, Green, Red, Green, Blue, Green, Green, Red, Green, Green, Blue, Green, Red, Green, Red, Green, Blue, Green, Blue, Green, Red, Green, Green, Red, Green, Green, Blue];
    let cfa = CfaPattern { width: 6, height: 6, entries: e.to_vec(), origin_x: 2, origin_y: 5 };
    ImageMetadata::with_cfa(w, h, SampleLayout::OtherCfa, cfa)
}

fn raw(d: Decoded) -> RawImage {
    match d {
        Decoded::Raw(r) => r,
        Decoded::Standard(_) => panic!("expected a RAW image"),
    }
}

/// Rewrite bytes and re-seal the header checksum so the semantic check (not the checksum) is what rejects the file.
fn patched_sealed(f: &[u8], at: usize, bytes: &[u8]) -> Vec<u8> {
    let mut v = f.to_vec();
    v[at..at + bytes.len()].copy_from_slice(bytes);
    reseal(&mut v);
    v
}
fn reseal(v: &mut [u8]) {
    let meta_len = u16::from_le_bytes([v[10], v[11]]) as usize;
    let mut c = Crc32::new();
    c.update(&v[..28]);
    c.update(&v[32..32 + meta_len]);
    v[28..32].copy_from_slice(&c.finish().to_le_bytes());
}

fn lisa_file() -> (Vec<u16>, ImageMetadata, Vec<u8>) {
    let m = bayer_meta(24, 18, [ColorFilter::Red, ColorFilter::Green, ColorFilter::Green, ColorFilter::Blue]);
    let s = raw_samples(24, 18, 12, 3);
    let f = encode_raw(&s, 24, 18, 12, m.clone()).unwrap();
    (s, m, f)
}

fn err(b: &[u8]) -> ContainerError {
    decode(b).expect_err("decode should fail")
}

// ---------------------------------------------------------------- standard images (no codec registered)

#[test]
fn standard_encoder_is_not_registered_until_ellie_is_wired_in() {
    assert!(matches!(encode_standard(&std_img(4, 4, Layout::Rgb, 1)), Err(ContainerError::UnsupportedImage(_))));
}

// ---------------------------------------------------------------- LISA RAW

#[test]
fn raw_round_trips_bayer_phases_depths_and_odd_sizes() {
    use ColorFilter::*;
    let phases = [[Red, Green, Green, Blue], [Blue, Green, Green, Red], [Green, Red, Blue, Green], [Green, Blue, Red, Green]];
    for (pi, e) in phases.iter().enumerate() {
        for (w, h, bd) in [(1usize, 1usize, 12u8), (3, 5, 8), (17, 9, 10), (32, 24, 14), (31, 25, 16), (9, 1, 12), (1, 9, 12)] {
            let meta = bayer_meta(w, h, *e);
            let s = raw_samples(w, h, bd, (pi * 100 + w) as u32);
            for mode in [Stage1Mode::BayerPlanar, Stage1Mode::Mosaic] {
                let f = encode_raw_with_mode(&s, w, h, bd, meta.clone(), mode).unwrap();
                let r = raw(decode(&f).unwrap());
                assert_eq!((r.bit_depth, &r.metadata), (bd, &meta), "phase {pi} {w}x{h} {bd}b {mode:?}");
                assert_eq!(r.samples, s, "phase {pi} {w}x{h} {bd}b {mode:?}");
                let LisaParamsView(stage1) = LisaParamsView::of(&f);
                assert_eq!(stage1, mode);
                assert_eq!(f, encode_raw_with_mode(&s, w, h, bd, meta.clone(), mode).unwrap(), "deterministic");
            }
        }
    }
}

struct LisaParamsView(Stage1Mode);
impl LisaParamsView {
    fn of(f: &[u8]) -> Self {
        match parse_header(f).unwrap().params {
            CodecParams::Lisa(p) => LisaParamsView(p.stage1),
            _ => panic!("not LISA"),
        }
    }
}

#[test]
fn raw_round_trips_non_bayer_layouts_and_other_cfa() {
    let (w, h, bd) = (37usize, 29usize, 12u8);
    let s = raw_samples(w, h, bd, 11);
    let metas = [
        ImageMetadata::without_cfa(w, h, SampleLayout::Monochrome),
        ImageMetadata::without_cfa(w, h, SampleLayout::Linear),
        ImageMetadata::without_cfa(w, h, SampleLayout::Unspecified),
        xtrans_like(w, h),
    ];
    for meta in metas {
        // `BayerPlanar` is requested but is only effective for Bayer metadata; the file records what was applied.
        let f = encode_raw(&s, w, h, bd, meta.clone()).unwrap();
        assert_eq!(LisaParamsView::of(&f).0, Stage1Mode::Mosaic);
        let r = raw(decode(&f).unwrap());
        assert_eq!((&r.metadata, &r.samples), (&meta, &s), "{:?}", meta.layout);
    }
}

#[test]
fn raw_bit_depth_range_and_sample_validation() {
    for bd in 1..=16u8 {
        let meta = ImageMetadata::without_cfa(8, 6, SampleLayout::Monochrome);
        let s = raw_samples(8, 6, bd, bd as u32);
        let f = encode_raw(&s, 8, 6, bd, meta).unwrap();
        assert_eq!(raw(decode(&f).unwrap()).samples, s, "bit depth {bd}");
    }
    let meta = ImageMetadata::without_cfa(2, 2, SampleLayout::Monochrome);
    assert!(encode_raw(&[0, 1, 2, 4096], 2, 2, 12, meta.clone()).is_err(), "sample above bit depth");
    assert!(encode_raw(&[0; 4], 2, 2, 17, meta.clone()).is_err());
    assert!(encode_raw(&[0; 3], 2, 2, 12, meta).is_err(), "length mismatch");
}

#[test]
fn raw_metadata_survives_serialization_exactly() {
    let m = xtrans_like(40, 30);
    let s = raw_samples(40, 30, 14, 2);
    let f = encode_raw(&s, 40, 30, 14, m.clone()).unwrap();
    let CodecParams::Lisa(p) = parse_header(&f).unwrap().params else { panic!() };
    assert_eq!(p.metadata, m);
    assert_eq!((p.stage3_rule_version, p.stage4_coding_version), (2, 1));
    assert_eq!(parse_header(&f).unwrap().meta_len as usize, 12 + 36);
}

// ---------------------------------------------------------------- rejection policy

#[test]
fn unsupported_container_versions_are_rejected_first() {
    let (_, _, f) = lisa_file();
    for v in [0u8, 3, 255] {
        let mut b = f.clone();
        b[4] = v;
        assert_eq!(err(&b), ContainerError::UnsupportedVersion(v));
    }
    assert_eq!(err(&[0x89, b'P', b'K']), ContainerError::Truncated { need: 5, have: 3 });
    assert_eq!(err(b"PNG!more"), ContainerError::BadMagic);
    assert_eq!(err(&[]), ContainerError::Truncated { need: 5, have: 0 });
}

#[test]
fn unknown_mandatory_features_and_codecs_are_rejected() {
    let (_, _, f) = lisa_file();
    for bit in 0..8 {
        let b = patched_sealed(&f, 5, &[1 << bit]);
        assert_eq!(err(&b), ContainerError::ReservedFlags(1 << bit));
    }
    for c in [0u8, 5, 99, 255] {
        assert_eq!(err(&patched_sealed(&f, 6, &[c])), ContainerError::UnsupportedCodec(c));
    }
    assert_eq!(err(&patched_sealed(&f, 7, &[2])), ContainerError::UnsupportedMetaVersion { codec: CODEC_LISA, version: 2 });
    assert_eq!(err(&patched_sealed(&f, 7, &[0])), ContainerError::UnsupportedMetaVersion { codec: CODEC_LISA, version: 0 });
}

#[test]
fn unsupported_codec_versions_are_rejected() {
    let (_, _, f) = lisa_file();
    let at = FIXED_HEADER_LEN;
    let unsupported = |field, value| ContainerError::UnsupportedCodecVersion { codec: CODEC_LISA, field, value };
    assert_eq!(err(&patched_sealed(&f, at, &[2])), unsupported("stage1_mode", 2));
    assert_eq!(err(&patched_sealed(&f, at + 1, &[9])), unsupported("sample_layout", 9));
    for v in [0u8, 1, 3, 4, 5] {
        assert_eq!(err(&patched_sealed(&f, at + 2, &[v])), unsupported("stage3_rule_version", v));
    }
    for v in [0u8, 2, 3] {
        assert_eq!(err(&patched_sealed(&f, at + 3, &[v])), unsupported("stage4_coding_version", v));
    }
}

#[test]
fn corrupt_codec_metadata_is_rejected() {
    let (_, _, f) = lisa_file();
    let at = FIXED_HEADER_LEN;
    // Layout code changed to a non-CFA layout: the CFA block is then trailing metadata.
    assert!(matches!(err(&patched_sealed(&f, at + 1, &[1])), ContainerError::InvalidMetaLength { .. }));
    // Mosaic fallback cannot be "planar" on non-Bayer data.
    let mono = encode_raw(&raw_samples(8, 8, 12, 1), 8, 8, 12, ImageMetadata::without_cfa(8, 8, SampleLayout::Monochrome)).unwrap();
    assert!(matches!(err(&patched_sealed(&mono, at, &[1])), ContainerError::InvalidMetadata(_)));
    // Layout changed to Bayer without a CFA block.
    assert!(matches!(err(&patched_sealed(&mono, at + 1, &[3])), ContainerError::InvalidMetaLength { .. }));
    // Bad CFA contents: invalid colour code, origin out of range, non-Bayer arrangement, zero repeat.
    assert!(matches!(err(&patched_sealed(&f, at + 12, &[7])), ContainerError::Lisa(_)));
    assert!(matches!(err(&patched_sealed(&f, at + 8, &[2, 0])), ContainerError::Lisa(_)));
    assert!(matches!(err(&patched_sealed(&f, at + 13, &[0])), ContainerError::Lisa(_)), "two reds is not Bayer");
    assert!(matches!(err(&patched_sealed(&f, at + 4, &[0, 0])), ContainerError::InvalidMetaLength { .. }));
    // Metadata block shorter/longer than the CFA says: also covered by meta_len edits.
    let mut short = f.clone();
    short[10..12].copy_from_slice(&3u16.to_le_bytes());
    reseal(&mut short);
    assert!(matches!(err(&short), ContainerError::InvalidMetaLength { .. }));
    // LISA must be single-channel and within 1..=16 bits.
    assert!(matches!(err(&patched_sealed(&f, 9, &[3])), ContainerError::InvalidChannels { .. }));
    assert!(matches!(err(&patched_sealed(&f, 8, &[0])), ContainerError::UnsupportedBitDepth { .. }));
    assert!(matches!(err(&patched_sealed(&f, 8, &[17])), ContainerError::UnsupportedBitDepth { .. }));
}

#[test]
fn header_checksum_covers_every_header_and_metadata_byte() {
    for f in [lisa_file().2] {
        let hdr = parse_header(&f).unwrap();
        let covered = FIXED_HEADER_LEN + hdr.meta_len as usize;
        for i in 0..covered {
            let mut b = f.clone();
            b[i] ^= 0x10;
            // Bytes 0..=5 are checked before the checksum (magic, version, flags); the rest must fail the header CRC
            // (or, for length fields that become inconsistent, be rejected as truncated).
            assert!(decode(&b).is_err(), "flip at byte {i} accepted");
            if i >= 6 && i != 10 && i != 11 {
                assert!(matches!(err(&b), ContainerError::HeaderChecksumMismatch { .. }), "byte {i}: {:?}", err(&b));
            }
        }
    }
}

#[test]
fn pixel_checksum_mismatch_is_reported() {
    for f in [lisa_file().2] {
        let crc = u32::from_le_bytes(f[24..28].try_into().unwrap());
        let b = patched_sealed(&f, 24, &(crc ^ 1).to_le_bytes());
        assert_eq!(err(&b), ContainerError::ChecksumMismatch { expected: crc ^ 1, actual: crc });
    }
}

#[test]
fn truncated_trailing_and_inconsistent_lengths_are_rejected() {
    for f in [lisa_file().2] {
        for cut in 0..f.len() {
            assert!(decode(&f[..cut]).is_err(), "prefix of {cut} bytes accepted");
        }
        let mut t = f.clone();
        t.push(0);
        assert_eq!(err(&t), ContainerError::TrailingBytes(1));
        let plen = u32::from_le_bytes(f[20..24].try_into().unwrap());
        assert!(matches!(err(&patched_sealed(&f, 20, &(plen + 1).to_le_bytes())), ContainerError::Truncated { .. }));
        assert!(matches!(err(&patched_sealed(&f, 20, &(plen - 1).to_le_bytes())), ContainerError::TrailingBytes(1)));
        assert!(matches!(err(&patched_sealed(&f, 20, &0u32.to_le_bytes())), ContainerError::InvalidPayloadLength { .. }));
        for (at, name) in [(12, "width"), (16, "height")] {
            assert!(matches!(err(&patched_sealed(&f, at, &0u32.to_le_bytes())), ContainerError::InvalidDimensions { .. }), "{name}");
        }
    }
}

#[test]
fn oversized_claims_are_rejected_before_any_decoding() {
    let (_, _, f) = lisa_file();
    // 40 M samples claimed by a ~hundreds-of-bytes payload: beyond the LISA samples-per-byte bound.
    let b = patched_sealed(&patched_sealed(&f, 12, &20_000u32.to_le_bytes()), 16, &2_000u32.to_le_bytes());
    assert!(matches!(err(&b), ContainerError::InvalidPayloadLength { .. }));
    // Decode limit applies to bytes of reconstructed samples (2 per sample above 8 bits).
    let big = patched_sealed(&patched_sealed(&f, 12, &(1u32 << 20).to_le_bytes()), 16, &(1u32 << 20).to_le_bytes());
    assert!(matches!(err(&big), ContainerError::TooLarge { .. } | ContainerError::Lisa(_)));
    assert!(matches!(decode_with_limit(&f, 10), Err(ContainerError::TooLarge { .. })));
    assert!(decode_with_limit(&f, 24 * 18 * 2).is_ok());
    assert!(matches!(decode_with_limit(&f, 24 * 18 * 2 - 1), Err(ContainerError::TooLarge { .. })));
    let overflow = patched_sealed(&patched_sealed(&f, 12, &u32::MAX.to_le_bytes()), 16, &u32::MAX.to_le_bytes());
    assert!(matches!(err(&overflow), ContainerError::TooLarge { .. }));
}

#[test]
fn payload_bound_holds_for_the_flattest_lisa_images() {
    // The bound must never reject a file this encoder wrote; a constant image is the densest case.
    for bd in [8u8, 12, 16] {
        let (w, h) = (2048usize, 2048usize);
        let meta = ImageMetadata::without_cfa(w, h, SampleLayout::Monochrome);
        let f = encode_raw(&vec![(1u16 << bd) / 2; w * h], w, h, bd, meta).unwrap();
        let hdr = parse_header(&f).unwrap();
        let ratio = (w * h) as f64 / hdr.payload_len as f64;
        println!("flat {bd}-bit {w}x{h}: payload {} B, {ratio:.0} samples/byte (bound 16384)", hdr.payload_len);
        assert!(ratio < 16384.0);
    }
}

#[test]
fn damaged_payloads_never_panic_and_never_decode_to_other_pixels() {
    let (s, _, f) = lisa_file();
    let start = FIXED_HEADER_LEN + parse_header(&f).unwrap().meta_len as usize;
    for i in start..f.len() {
        for bit in [0x01u8, 0x80] {
            let mut b = f.clone();
            b[i] ^= bit;
            match decode(&b) {
                Err(_) => {}
                Ok(d) => assert_eq!(raw(d).samples, s, "flip at {i} decoded to different pixels"),
            }
        }
    }
}

// ---------------------------------------------------------------- golden v2 files (spec examples)

fn golden_sources() -> Vec<(&'static str, Vec<u8>)> {
    let bayer = encode_raw(&raw_samples(16, 10, 12, 4), 16, 10, 12, bayer_meta(16, 10, [ColorFilter::Green, ColorFilter::Red, ColorFilter::Blue, ColorFilter::Green])).unwrap();
    let xt = encode_raw(&raw_samples(14, 9, 14, 6), 14, 9, 14, xtrans_like(14, 9)).unwrap();
    vec![("lisa_grbg_16x10_12bit.pkon", bayer), ("lisa_othercfa_14x9_14bit.pkon", xt)]
}

#[test]
fn golden_v2_files_decode_and_reencode_byte_exact() {
    let dir = std::path::Path::new("tests/golden_pkon");
    if std::env::var_os("PKON_WRITE_GOLDEN").is_some() {
        std::fs::create_dir_all(dir).unwrap();
        for (name, bytes) in golden_sources() {
            std::fs::write(dir.join(name), bytes).unwrap();
        }
    }
    for (name, expected) in golden_sources() {
        let on_disk = std::fs::read(dir.join(name)).unwrap_or_else(|e| panic!("{name}: {e} (run with PKON_WRITE_GOLDEN=1 once)"));
        assert_eq!(on_disk, expected, "{name}: encoder output changed");
        assert!(decode(&on_disk).is_ok(), "{name}");
    }
}

#[test]
#[ignore]
fn print_spec_examples() {
    for (name, b) in golden_sources() {
        let hdr = parse_header(&b).unwrap();
        let n = FIXED_HEADER_LEN + hdr.meta_len as usize;
        let hex = |s: &[u8]| s.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ");
        println!("{name}: {} bytes\n header: {}\n meta:   {}\n payload[..8]: {}\n {hdr:?}", b.len(), hex(&b[..FIXED_HEADER_LEN]), hex(&b[FIXED_HEADER_LEN..n]), hex(&b[n..(n + 8).min(b.len())]));
    }
}

// ---------------------------------------------------------------- real DNG corpus (skipped when the prepared data is absent)

#[test]
fn real_dng_corpus_crops_round_trip() {
    // (id, width, height, bit depth, cfa codes, cfa dim): top-left crops of the prepared canonical mosaics.
    let cases: [(&str, usize, usize, u8, &[u8], usize); 4] = [
        ("bmpcc4k_person", 4128, 2176, 12, &[0, 1, 1, 2], 2),
        ("leica_m8_house", 3920, 2638, 8, &[0, 1, 1, 2], 2),
        ("hasselblad_tallinn", 5568, 3648, 16, &[0, 1, 1, 2], 2),
        ("x100s_xtrans_chart", 4936, 3296, 14, &[0, 1, 0, 2, 1, 2, 1, 2, 1, 1, 0, 1, 1, 0, 1, 1, 2, 1, 2, 1, 2, 0, 1, 0, 1, 0, 1, 1, 2, 1, 1, 2, 1, 1, 0, 1], 6),
    ];
    let (cw, ch) = (640usize, 480usize);
    let mut ran = 0;
    for (id, w, h, bd, cfa, dim) in cases {
        let Ok(bytes) = std::fs::read(format!("data/praw1/canon/{id}.u16")) else {
            println!("{id}: prepared data absent, skipped");
            continue;
        };
        assert_eq!(bytes.len(), w * h * 2, "{id}");
        let mut s = Vec::with_capacity(cw * ch);
        for y in 0..ch {
            let row = &bytes[y * w * 2..(y * w + cw) * 2];
            s.extend(row.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])));
        }
        let entries: Vec<ColorFilter> = cfa.iter().map(|&c| ColorFilter::try_from(c).unwrap()).collect();
        let pat = CfaPattern { width: dim, height: dim, entries, origin_x: 0, origin_y: 0 };
        let bayer = ImageMetadata::with_cfa(cw, ch, SampleLayout::BayerCfa, pat.clone());
        let meta = if dim == 2 && bayer.validate().is_ok() { bayer } else { ImageMetadata::with_cfa(cw, ch, SampleLayout::OtherCfa, pat) };
        let f = encode_raw(&s, cw, ch, bd, meta.clone()).unwrap();
        let r = raw(decode(&f).unwrap());
        assert!(r.samples == s && r.metadata == meta && r.bit_depth == bd, "{id}");
        println!("{id} {bd}b crop {cw}x{ch}: {} B -> {} B file", s.len() * 2, f.len());
        ran += 1;
    }
    println!("real corpus crops verified: {ran}/4");
}

#[test]
fn raw_files_dispatch_to_the_raw_variant() {
    let (_, _, lf) = lisa_file();
    assert!(matches!(decode(&lf).unwrap(), Decoded::Raw(_)));
    assert_eq!((lf[4], lf[6]), (VERSION, CODEC_LISA));
}

/// The retired PKON2 codec ids (1..=3) are rejected, in a version-2 header and for version-1 files.
#[test]
fn retired_pkon2_codecs_are_unsupported() {
    let (_, _, lf) = lisa_file();
    for codec in 1..=3u8 {
        assert_eq!(err(&patched_sealed(&lf, 6, &[codec])), ContainerError::UnsupportedCodec(codec));
        let mut v1 = [0u8; pkonfile::HEADER_LEN];
        v1[..4].copy_from_slice(&MAGIC);
        v1[4] = VERSION_V1;
        v1[6] = codec;
        v1[7] = 2; // RGB
        v1[8..12].copy_from_slice(&4u32.to_le_bytes());
        v1[12..16].copy_from_slice(&4u32.to_le_bytes());
        v1[16..20].copy_from_slice(&8u32.to_le_bytes());
        assert_eq!(container_version(&v1).unwrap(), VERSION_V1);
        assert_eq!(err(&v1), ContainerError::V1(pkonfile::PkonError::UnsupportedCodec(codec)));
    }
}
