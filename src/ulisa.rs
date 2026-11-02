//! ULISA: one image compression system over two independent engines, LISA and ELLIE.
//!
//! This module only coordinates. It never touches predictors, transforms or entropy coders: encoding calls
//! `pkon::encode_raw` (LISA, codec id 4) or `pkon::encode_standard_with` (ELLIE, codec id 6), and decoding goes through
//! `pkon::decode`. The existing `.pkon` v2 container already carries the engine id in its header, so a ULISA file is
//! simply a `.pkon` file and every existing standalone Lisa/ELLIE file is already a valid ULISA file.
//!
//! Whole-image engine selection only. An engine is never substituted for another: input an engine does not accept is an
//! error (LISA takes RAW sample planes, ELLIE takes decoded-pixel standard images).
use crate::canon::Canonical;
use crate::lisa::ImageMetadata;
use crate::pkon::{self, ContainerError, Decoded, EllieRepr, CODEC_ELLIE, CODEC_LISA};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    Lisa,
    Ellie,
}

impl Engine {
    /// The `.pkon` v2 codec id that identifies this engine on disk.
    pub fn codec_id(self) -> u8 {
        match self {
            Engine::Lisa => CODEC_LISA,
            Engine::Ellie => CODEC_ELLIE,
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
    /// The file's codec id belongs to neither engine.
    UnknownEngine(u8),
    Container(ContainerError),
}

impl fmt::Display for UlisaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UlisaError::EngineSourceMismatch { engine, source } => write!(f, "{engine:?} cannot encode a {source} source"),
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

/// Encode with an explicitly chosen engine.
pub fn encode(engine: Engine, source: &Source) -> Result<Vec<u8>, UlisaError> {
    match (engine, source) {
        (Engine::Lisa, Source::Raw { samples, width, height, bit_depth, metadata }) => {
            Ok(pkon::encode_raw(samples, *width, *height, *bit_depth, metadata.clone())?)
        }
        (Engine::Ellie, Source::Standard(img)) => Ok(pkon::encode_standard_with(img, EllieRepr::Auto)?),
        (engine, Source::Raw { .. }) => Err(UlisaError::EngineSourceMismatch { engine, source: "RAW" }),
        (engine, Source::Standard(_)) => Err(UlisaError::EngineSourceMismatch { engine, source: "standard" }),
    }
}

/// Which engine wrote this file. Validates the whole header (magic, version, checksum, engine metadata) but decodes no pixels.
pub fn engine_of(bytes: &[u8]) -> Result<Engine, UlisaError> {
    match pkon::parse_header(bytes)?.codec {
        CODEC_LISA => Ok(Engine::Lisa),
        CODEC_ELLIE => Ok(Engine::Ellie),
        id => Err(UlisaError::UnknownEngine(id)),
    }
}

/// Decode any ULISA (= `.pkon` v2) file with the engine it names. Returns the engine alongside the pixels.
pub fn decode(bytes: &[u8]) -> Result<(Engine, Decoded), UlisaError> {
    let engine = engine_of(bytes)?;
    Ok((engine, pkon::decode(bytes)?))
}
