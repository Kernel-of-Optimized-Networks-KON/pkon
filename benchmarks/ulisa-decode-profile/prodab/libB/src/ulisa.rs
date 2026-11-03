//! ULISA: one image compression system over two engines, LISA and ELLIE.
//!
//! This module only coordinates. It never touches predictors, transforms or entropy coders: encoding calls
//! `pkon::encode_raw` (LISA, codec id 4), `pkon::encode_standard_with` (ELLIE, codec id 6) or `pkon::encode_ulisa_report`
//! (the unified Bayer codec, id 7: LISA builds the sensor frame, ELLIE codes it), and decoding goes through `pkon::decode`.
//! The `.pkon` v2 container carries the codec id in its header, so every standalone Lisa/ELLIE file is already a valid ULISA file.
//!
//! Engine selection is explicit. LISA and ELLIE are never substituted for one another: input an engine does not accept is an
//! error. The single deliberate exception is `Engine::Ulisa` on a RAW mosaic the 2x2 Bayer frame cannot represent (other CFAs,
//! mosaics smaller than 2x2): it falls back to LISA's RAW path, and says so in [`Encoded::fallback`] and in the file's codec id
//! (`engine_of`). [`Fallback::None`] turns that fallback into an error. Errors other than "unsupported CFA/geometry" never fall back.
use crate::canon::Canonical;
use crate::lisa::ImageMetadata;
use crate::lisa_frame;
use crate::pkon::{self, ContainerError, Decoded, EllieRepr, UlisaEncodeInfo, CODEC_ELLIE, CODEC_LISA, CODEC_ULISA};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    Lisa,
    Ellie,
    /// Unified Bayer RAW codec (codec 7): LISA sensor frame, ELLIE coding loop.
    Ulisa,
}

impl Engine {
    /// The `.pkon` v2 codec id that identifies this engine on disk.
    pub fn codec_id(self) -> u8 {
        match self {
            Engine::Lisa => CODEC_LISA,
            Engine::Ellie => CODEC_ELLIE,
            Engine::Ulisa => CODEC_ULISA,
        }
    }
}

/// Input to ULISA, in the form each engine natively takes.
pub enum Source<'a> {
    /// RAW mosaic or plane for LISA (this is the DNG path).
    Raw { samples: &'a [u16], width: usize, height: usize, bit_depth: u8, metadata: ImageMetadata },
    /// Decoded-pixel image for ELLIE.
    Standard(&'a Canonical),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UlisaError {
    /// The requested engine cannot encode this kind of source (no silent fallback to the other engine).
    EngineSourceMismatch { engine: Engine, source: &'static str },
    /// `Engine::Ulisa` with [`Fallback::None`] on a mosaic the 2x2 Bayer frame cannot represent.
    BayerUnsupported(&'static str),
    /// The file's codec id belongs to no engine.
    UnknownEngine(u8),
    Container(ContainerError),
}

impl fmt::Display for UlisaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UlisaError::EngineSourceMismatch { engine, source } => write!(f, "{engine:?} cannot encode a {source} source"),
            UlisaError::BayerUnsupported(why) => write!(f, "ULISA Bayer frame unsupported: {why}"),
            UlisaError::UnknownEngine(id) => write!(f, "codec id {id} is not a ULISA engine"),
            UlisaError::Container(e) => write!(f, "{e}"),
        }
    }
}
impl std::error::Error for UlisaError {}
impl From<ContainerError> for UlisaError {
    fn from(e: ContainerError) -> Self {
        UlisaError::Container(e)
    }
}

/// What to do when `Engine::Ulisa` is asked to encode a mosaic the Bayer frame cannot represent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fallback {
    /// Encode with LISA's RAW path instead and report it (the default of [`encode`] and [`encode_report`]).
    Lisa,
    /// Fail with [`UlisaError::BayerUnsupported`].
    None,
}

/// Result of an encode: the file and which engine actually wrote it.
#[derive(Clone, Debug)]
pub struct Encoded {
    pub bytes: Vec<u8>,
    pub requested: Engine,
    pub used: Engine,
    /// Why `used != requested`, when the Lisa fallback was taken.
    pub fallback: Option<&'static str>,
    /// ULISA only: whether L1 activated, the coding depth, and how often the flank context was non-neutral.
    pub ulisa: Option<UlisaEncodeInfo>,
}

/// Encode with an explicitly chosen engine. `Engine::Ulisa` falls back to LISA on an unsupported CFA (see the module comment);
/// use [`encode_report`] to learn whether it did, or [`encode_with`] with [`Fallback::None`] to forbid it.
pub fn encode(engine: Engine, source: &Source) -> Result<Vec<u8>, UlisaError> {
    encode_report(engine, source).map(|e| e.bytes)
}

pub fn encode_report(engine: Engine, source: &Source) -> Result<Encoded, UlisaError> {
    encode_with(engine, source, Fallback::Lisa)
}

pub fn encode_with(engine: Engine, source: &Source, fallback: Fallback) -> Result<Encoded, UlisaError> {
    let done = |bytes, used, fallback, ulisa| Encoded { bytes, requested: engine, used, fallback, ulisa };
    match (engine, source) {
        (Engine::Lisa, Source::Raw { samples, width, height, bit_depth, metadata }) => {
            Ok(done(pkon::encode_raw(samples, *width, *height, *bit_depth, metadata.clone())?, Engine::Lisa, None, None))
        }
        (Engine::Ellie, Source::Standard(img)) => Ok(done(pkon::encode_standard_with(img, EllieRepr::Auto)?, Engine::Ellie, None, None)),
        (Engine::Ulisa, Source::Raw { samples, width, height, bit_depth, metadata }) => match lisa_frame::check_supported(metadata) {
            Ok(()) => {
                let (bytes, info) = pkon::encode_ulisa_report(samples, *width, *height, *bit_depth, metadata.clone())?;
                Ok(done(bytes, Engine::Ulisa, None, Some(info)))
            }
            Err(lisa_frame::FrameError::Unsupported(why)) => match fallback {
                Fallback::Lisa => {
                    Ok(done(pkon::encode_raw(samples, *width, *height, *bit_depth, metadata.clone())?, Engine::Lisa, Some(why), None))
                }
                Fallback::None => Err(UlisaError::BayerUnsupported(why)),
            },
            Err(e) => Err(UlisaError::Container(ContainerError::InvalidMetadata(e.to_string()))),
        },
        (engine, Source::Raw { .. }) => Err(UlisaError::EngineSourceMismatch { engine, source: "RAW" }),
        (engine, Source::Standard(_)) => Err(UlisaError::EngineSourceMismatch { engine, source: "standard" }),
    }
}

/// Which engine wrote this file. Validates the whole header (magic, version, checksum, engine metadata) but decodes no pixels.
pub fn engine_of(bytes: &[u8]) -> Result<Engine, UlisaError> {
    match pkon::parse_header(bytes)?.codec {
        CODEC_LISA => Ok(Engine::Lisa),
        CODEC_ELLIE => Ok(Engine::Ellie),
        CODEC_ULISA => Ok(Engine::Ulisa),
        id => Err(UlisaError::UnknownEngine(id)),
    }
}

/// Decode any ULISA (= `.pkon` v2) file with the engine it names. Returns the engine alongside the pixels.
pub fn decode(bytes: &[u8]) -> Result<(Engine, Decoded), UlisaError> {
    let engine = engine_of(bytes)?;
    Ok((engine, pkon::decode(bytes)?))
}
