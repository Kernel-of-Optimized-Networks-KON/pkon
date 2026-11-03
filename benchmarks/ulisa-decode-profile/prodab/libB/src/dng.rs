//! DNG ingestion for the converter UI: DNG bytes -> the exact stored `u16` raster + the metadata LISA needs.
//!
//! There is no DNG parser in Rust; the repo's reader is `tools/lisa_dng.py` (tifffile). This module runs the thin wrapper
//! `tools/lisa_dng_extract.py` as a subprocess and maps its output onto `ImageMetadata`. Nothing is cropped, scaled,
//! black-level-subtracted, white-balanced or demosaiced. The layout decision is the one `bench_lisa1` uses: a valid 2x2
//! Bayer pattern -> `BayerCfa`, any other CFA -> `OtherCfa`, no CFA -> `Monochrome`.
use crate::lisa::{CfaPattern, ColorFilter, ImageMetadata, SampleLayout};
use std::io::{Read, Write};
use std::process::{Command, Stdio};

pub struct DngInput {
    pub samples: Vec<u16>,
    pub bit_depth: u8,
    /// BitsPerSample as stored in the DNG (`bit_depth` can be smaller when the data and WhiteLevel fit fewer bits).
    pub declared_bits: u8,
    pub metadata: ImageMetadata,
    pub camera: String,
    pub dng_compression: String,
}

/// True for a TIFF/DNG header (`II*\0` or `MM\0*`).
pub fn looks_like_tiff(bytes: &[u8]) -> bool {
    bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*")
}

fn python() -> String {
    std::env::var("PKON_PYTHON").unwrap_or_else(|_| if cfg!(windows) { "python".into() } else { "python3".into() })
}

pub fn ingest_dng(bytes: &[u8]) -> Result<DngInput, String> {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tools/lisa_dng_extract.py");
    let mut child = Command::new(python())
        .args(["-I", script])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot start Python for DNG ingestion ({e}); install Python with numpy, tifffile and imagecodecs, or set PKON_PYTHON"))?;
    let mut stdin = child.stdin.take().unwrap();
    let (mut stdout, mut stderr) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
    // Feed stdin from a thread so a large DNG cannot deadlock against the child's output pipe.
    let (out, err) = std::thread::scope(|s| {
        s.spawn(move || {
            let _ = stdin.write_all(bytes);
        });
        let e = s.spawn(move || {
            let mut v = Vec::new();
            let _ = stderr.read_to_end(&mut v);
            v
        });
        let mut o = Vec::new();
        let r = stdout.read_to_end(&mut o);
        (r.map(|_| o), e.join().unwrap_or_default())
    });
    let status = child.wait().map_err(|e| e.to_string())?;
    let err = String::from_utf8_lossy(&err).trim().to_string();
    if !status.success() {
        let first = err.lines().last().unwrap_or("unknown error");
        return Err(if status.code() == Some(2) { first.to_string() } else { format!("DNG ingestion failed: {first}") });
    }
    parse_extract(&out.map_err(|e| e.to_string())?)
}

fn parse_extract(out: &[u8]) -> Result<DngInput, String> {
    let nl = out.iter().position(|&b| b == b'\n').ok_or("DNG ingestion produced no metadata")?;
    let m: serde_json::Value = serde_json::from_slice(&out[..nl]).map_err(|e| format!("bad DNG metadata: {e}"))?;
    let u = |k: &str| m[k].as_u64().ok_or_else(|| format!("DNG metadata missing {k}"));
    let (w, h) = (u("width")? as usize, u("height")? as usize);
    let bit_depth = u8::try_from(u("bit_depth")?).map_err(|_| "bad bit depth")?;
    let declared_bits = u8::try_from(u("declared_bits")?).map_err(|_| "bad declared bit depth")?;
    let raw = &out[nl + 1..];
    if raw.len() != w.checked_mul(h).and_then(|n| n.checked_mul(2)).ok_or("dimensions overflow")? {
        return Err(format!("DNG sample data is {} bytes, expected {}", raw.len(), w * h * 2));
    }
    let samples: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let ints = |k: &str| -> Option<Vec<usize>> { m[k].as_array().map(|a| a.iter().filter_map(|v| v.as_u64().map(|x| x as usize)).collect()) };
    let metadata = match ints("cfa").filter(|c| !c.is_empty()) {
        None => ImageMetadata::without_cfa(w, h, SampleLayout::Monochrome),
        Some(cfa) => {
            let (dim, org) = (ints("cfa_dim").ok_or("DNG has CFAPattern without CFARepeatPatternDim")?, ints("cfa_origin").ok_or("missing CFA origin")?);
            if dim.len() != 2 || org.len() != 2 {
                return Err("malformed CFA dimensions".into());
            }
            let entries = cfa
                .iter()
                .map(|&v| u8::try_from(v).ok().and_then(|v| ColorFilter::try_from(v).ok()).ok_or(format!("unsupported CFA color code {v}")))
                .collect::<Result<Vec<_>, _>>()?;
            let pat = CfaPattern { width: dim[1], height: dim[0], entries, origin_x: org[0], origin_y: org[1] };
            let bayer = ImageMetadata::with_cfa(w, h, SampleLayout::BayerCfa, pat.clone());
            if dim == [2, 2] && bayer.validate().is_ok() { bayer } else { ImageMetadata::with_cfa(w, h, SampleLayout::OtherCfa, pat) }
        }
    };
    Ok(DngInput {
        samples,
        bit_depth,
        declared_bits,
        metadata,
        camera: m["camera"].as_str().unwrap_or("").to_string(),
        dng_compression: m["dng_compression"].as_str().unwrap_or("").to_string(),
    })
}
