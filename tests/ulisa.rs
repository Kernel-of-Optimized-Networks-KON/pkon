//! ULISA coordination: explicit engine routing, engine identification, standalone-file compatibility, no silent fallback.
use nanoscope::canon::{Canonical, Layout, Spec};
use nanoscope::lisa::{ImageMetadata, SampleLayout};
use nanoscope::pkon::{self, Decoded};
use nanoscope::ulisa::{self, Engine, Source, UlisaError};

fn raw() -> (Vec<u16>, ImageMetadata) {
    let (w, h) = (24usize, 16usize);
    let s: Vec<u16> = (0..w * h).map(|i| ((i * 7 + (i / w) * 13) % 4096) as u16).collect();
    (s, ImageMetadata::without_cfa(w, h, SampleLayout::Monochrome))
}

fn std_img() -> Canonical {
    let (w, h) = (16u32, 12u32);
    let data = (0..w * h * 3).map(|i| (i * 3 % 251) as u8).collect();
    Canonical::new(Spec { width: w, height: h, layout: Layout::Rgb, bit_depth: 8 }, data).unwrap()
}

#[test]
fn lisa_roundtrip_identified_and_standalone_compatible() {
    let (s, m) = raw();
    let src = Source::Raw { samples: &s, width: 24, height: 16, bit_depth: 12, metadata: m.clone() };
    let f = ulisa::encode(Engine::Lisa, &src).unwrap();
    assert_eq!(f, pkon::encode_raw(&s, 24, 16, 12, m).unwrap(), "ULISA must emit the standalone Lisa bytes");
    assert_eq!(ulisa::engine_of(&f).unwrap(), Engine::Lisa);
    match ulisa::decode(&f).unwrap() {
        (Engine::Lisa, Decoded::Raw(r)) => assert_eq!(r.samples, s),
        _ => panic!("wrong engine or payload kind"),
    }
}

#[test]
fn ellie_roundtrip_identified_and_standalone_compatible() {
    let img = std_img();
    let f = ulisa::encode(Engine::Ellie, &Source::Standard(&img)).unwrap();
    assert_eq!(f, pkon::encode_standard(&img).unwrap(), "ULISA must emit the standalone ELLIE bytes");
    assert_eq!(ulisa::engine_of(&f).unwrap(), Engine::Ellie);
    match ulisa::decode(&f).unwrap() {
        (Engine::Ellie, Decoded::Standard(c)) => assert_eq!(c.data, img.data),
        _ => panic!("wrong engine or payload kind"),
    }
}

#[test]
fn mismatched_engine_and_source_is_an_error_not_a_fallback() {
    let (s, m) = raw();
    let img = std_img();
    let raw_src = Source::Raw { samples: &s, width: 24, height: 16, bit_depth: 12, metadata: m };
    assert!(matches!(ulisa::encode(Engine::Ellie, &raw_src), Err(UlisaError::EngineSourceMismatch { engine: Engine::Ellie, .. })));
    assert!(matches!(ulisa::encode(Engine::Lisa, &Source::Standard(&img)), Err(UlisaError::EngineSourceMismatch { engine: Engine::Lisa, .. })));
}

#[test]
fn garbage_and_corrupt_headers_are_rejected() {
    assert!(ulisa::engine_of(b"not a pkon file at all, nope....").is_err());
    let img = std_img();
    let mut f = ulisa::encode(Engine::Ellie, &Source::Standard(&img)).unwrap();
    f[6] = 5; // codec id 5 is unassigned; header checksum or codec check must refuse it
    assert!(ulisa::engine_of(&f).is_err());
}
