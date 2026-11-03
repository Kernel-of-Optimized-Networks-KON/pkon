//! Round-trip verification: encode, decode from bytes only, compare every sample.
use crate::canon::{first_mismatch, Canonical, Mismatch};
use crate::codec::{Codec, CodecError};
use std::time::{Duration, Instant};

pub enum Verdict {
    Exact,
    Mismatch(Mismatch),
}

pub struct Trip {
    pub encoded: Vec<u8>,
    /// Best (minimum) over `reps`. Timed region is exactly `Codec::encode` / `Codec::decode`;
    /// comparison and hashing are outside it.
    pub enc_time: Duration,
    pub dec_time: Duration,
    pub verdict: Verdict,
}

pub enum TripError {
    Unsupported(String),
    Encode(String),
    Decode(String),
}

impl std::fmt::Display for TripError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TripError::Unsupported(s) => write!(f, "unsupported: {s}"),
            TripError::Encode(s) => write!(f, "encode failed: {s}"),
            TripError::Decode(s) => write!(f, "decode failed: {s}"),
        }
    }
}

/// The decoder is only ever handed the encoded bytes and the image's format hint; the
/// original pixels are never reachable from `decode`.
pub fn round_trip(codec: &dyn Codec, img: &Canonical, reps: u32) -> Result<Trip, TripError> {
    let (mut best_enc, mut best_dec) = (Duration::MAX, Duration::MAX);
    let mut first: Option<(Vec<u8>, Verdict)> = None;
    for _ in 0..reps.max(1) {
        let t = Instant::now();
        let encoded = codec.encode(img).map_err(|e| match e {
            CodecError::Unsupported(s) => TripError::Unsupported(s),
            CodecError::Failed(s) => TripError::Encode(s),
        })?;
        best_enc = best_enc.min(t.elapsed());
        let t = Instant::now();
        let decoded = codec.decode(&encoded, &img.spec).map_err(|e| TripError::Decode(e.to_string()))?;
        best_dec = best_dec.min(t.elapsed());
        let verdict = match first_mismatch(img, &decoded) {
            None => Verdict::Exact,
            Some(m) => Verdict::Mismatch(m),
        };
        match (&first, &verdict) {
            (None, _) | (Some((_, Verdict::Exact)), Verdict::Mismatch(_)) => first = Some((encoded, verdict)),
            _ => {}
        }
    }
    let (encoded, verdict) = first.unwrap();
    Ok(Trip { encoded, enc_time: best_enc, dec_time: best_dec, verdict })
}

/// Anti-cheat: a decoder that really depends on the stream cannot reproduce the original
/// image from half of it. Err means decode ignored (or could not notice damage to) its input.
pub fn truncation_check(codec: &dyn Codec, img: &Canonical, encoded: &[u8]) -> Result<(), String> {
    let cut = &encoded[..encoded.len() / 2];
    match codec.decode(cut, &img.spec) {
        Err(_) => Ok(()),
        Ok(d) if first_mismatch(img, &d).is_some() => Ok(()),
        Ok(_) => Err(format!("decode of {} of {} bytes still reproduced the original", cut.len(), encoded.len())),
    }
}
