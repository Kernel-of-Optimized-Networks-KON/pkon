//! External baselines (JPEG XL lossless, WebP lossless) driven through the `ffmpeg` CLI.
//!
//! Why ffmpeg: it is what is installed here, and it bundles libjxl and libwebp. It is a
//! stopgap; the reference JXL baseline should be `cjxl`/`djxl` (see README). Guard rails:
//! - we always pass an explicit input AND output `-pix_fmt`, so ffmpeg never inserts a
//!   silent colour conversion (the one exception, rgb24<->bgra for WebP, is exact);
//! - no codec is trusted to be lossless because of its flags: the harness diffs every sample.
//! - timings include process spawn + temp-file I/O, so they are NOT comparable with
//!   in-process codecs. The bench reports a no-op spawn calibration next to them.
use crate::canon::{Canonical, Layout, Spec};
use crate::codec::{Codec, CodecError};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use std::sync::atomic::{AtomicU32, Ordering};

#[derive(Clone, Copy)]
pub enum Kind {
    Jxl { effort: u8 },
    Webp { level: u8 },
}

pub struct Ffmpeg {
    pub kind: Kind,
    pub exe: String,
}

impl Ffmpeg {
    pub fn jxl(effort: u8) -> Self {
        Ffmpeg { kind: Kind::Jxl { effort }, exe: exe() }
    }
    pub fn webp(level: u8) -> Self {
        Ffmpeg { kind: Kind::Webp { level }, exe: exe() }
    }
}

pub fn exe() -> String {
    std::env::var("NANOSCOPE_FFMPEG").unwrap_or_else(|_| "ffmpeg".into())
}

/// First line of `ffmpeg -version`, or None if ffmpeg cannot be run.
pub fn ffmpeg_version() -> Option<String> {
    let out = Command::new(exe()).arg("-version").output().ok()?;
    String::from_utf8_lossy(&out.stdout).lines().next().map(|s| s.trim().to_string())
}

fn pix_fmt(spec: &Spec) -> &'static str {
    match (spec.layout, spec.bit_depth) {
        (Layout::Gray, 8) => "gray",
        (Layout::GrayAlpha, 8) => "ya8",
        (Layout::Rgb, 8) => "rgb24",
        (Layout::Rgba, 8) => "rgba",
        (Layout::Gray, _) => "gray16le",
        (Layout::GrayAlpha, _) => "ya16le",
        (Layout::Rgb, _) => "rgb48le",
        (Layout::Rgba, _) => "rgba64le",
    }
}

struct TempDir(PathBuf);
impl TempDir {
    fn new() -> Result<Self, CodecError> {
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!("nanoscope-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&p).map_err(|e| CodecError::Failed(e.to_string()))?;
        Ok(TempDir(p))
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Ffmpeg {
    /// `-xerror` is essential: without it ffmpeg's libjxl decoder retries a truncated stream
    /// forever (found the hard way: infinite stderr spam, >1 GB). The watchdog is the backstop.
    fn run(&self, tmp: &TempDir, args: &[&str]) -> Result<(), CodecError> {
        let errfile = tmp.0.join("stderr.txt");
        let mut child = Command::new(&self.exe)
            .args(["-hide_banner", "-loglevel", "error", "-xerror", "-y", "-nostdin"])
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&errfile).map_err(|e| CodecError::Failed(e.to_string()))?)
            .spawn()
            .map_err(|e| CodecError::Failed(format!("spawn {}: {e}", self.exe)))?;
        let deadline = Instant::now() + Duration::from_secs(30);
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) if Instant::now() > deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(CodecError::Failed("ffmpeg timed out after 30s (killed)".into()));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(1)),
                Err(e) => return Err(CodecError::Failed(e.to_string())),
            }
        };
        if status.success() {
            Ok(())
        } else {
            let msg = std::fs::read_to_string(&errfile).unwrap_or_default();
            Err(CodecError::Failed(msg.lines().last().unwrap_or("ffmpeg failed").trim().to_string()))
        }
    }
}

impl Codec for Ffmpeg {
    fn name(&self) -> String {
        match self.kind {
            Kind::Jxl { effort } => format!("jxl-e{effort}(ffmpeg)"),
            Kind::Webp { level } => format!("webp-l{level}(ffmpeg)"),
        }
    }
    fn version(&self) -> String {
        ffmpeg_version().unwrap_or_else(|| "unavailable".into())
    }
    fn config(&self) -> String {
        match self.kind {
            Kind::Jxl { effort } => format!("-c:v libjxl -distance 0 -effort {effort} (lossless)"),
            Kind::Webp { level } => format!("-c:v libwebp -lossless 1 -compression_level {level} -quality 100 -pix_fmt bgra"),
        }
    }
    fn available(&self) -> Result<(), String> {
        let out = Command::new(&self.exe)
            .args(["-hide_banner", "-encoders"])
            .output()
            .map_err(|e| format!("cannot run `{}`: {e}", self.exe))?;
        let text = String::from_utf8_lossy(&out.stdout);
        let need = match self.kind {
            Kind::Jxl { .. } => " libjxl ",
            Kind::Webp { .. } => " libwebp ",
        };
        if text.contains(need) {
            Ok(())
        } else {
            Err(format!("ffmpeg lacks encoder{}", need.trim_end()))
        }
    }
    fn supports(&self, spec: &Spec) -> bool {
        match self.kind {
            Kind::Jxl { .. } => true,
            // libwebp lossless is 8-bit RGB(A) only; gray/16-bit would need a conversion.
            Kind::Webp { .. } => spec.bit_depth == 8 && matches!(spec.layout, Layout::Rgb | Layout::Rgba),
        }
    }
    fn encode(&self, img: &Canonical) -> Result<Vec<u8>, CodecError> {
        if !self.supports(&img.spec) {
            return Err(CodecError::Unsupported(format!("{} not supported by {}", img.spec.label(), self.name())));
        }
        let tmp = TempDir::new()?;
        let (inp, outp) = (tmp.0.join("in.raw"), tmp.0.join(if matches!(self.kind, Kind::Jxl { .. }) { "o.jxl" } else { "o.webp" }));
        std::fs::write(&inp, &img.data).map_err(|e| CodecError::Failed(e.to_string()))?;
        let size = format!("{}x{}", img.spec.width, img.spec.height);
        let fmt = pix_fmt(&img.spec);
        let (inp_s, outp_s) = (inp.to_string_lossy().to_string(), outp.to_string_lossy().to_string());
        let mut args: Vec<String> = ["-f", "rawvideo", "-pix_fmt", fmt, "-video_size", &size, "-i", &inp_s, "-frames:v", "1"]
            .map(String::from)
            .to_vec();
        match self.kind {
            Kind::Jxl { effort } => args.extend(
                ["-c:v", "libjxl", "-distance", "0", "-effort", &effort.to_string(), "-pix_fmt", fmt].map(String::from),
            ),
            Kind::Webp { level } => args.extend(
                ["-c:v", "libwebp", "-lossless", "1", "-compression_level", &level.to_string(), "-quality", "100", "-pix_fmt", "bgra"]
                    .map(String::from),
            ),
        }
        args.extend(["-update", "1", "-f", "image2", &outp_s].map(String::from));
        self.run(&tmp, &args.iter().map(String::as_str).collect::<Vec<_>>())?;
        std::fs::read(&outp).map_err(|e| CodecError::Failed(e.to_string()))
    }
    fn decode(&self, enc: &[u8], hint: &Spec) -> Result<Canonical, CodecError> {
        let tmp = TempDir::new()?;
        let ext = if matches!(self.kind, Kind::Jxl { .. }) { "jxl" } else { "webp" };
        let (inp, outp) = (tmp.0.join(format!("i.{ext}")), tmp.0.join("out.raw"));
        std::fs::write(&inp, enc).map_err(|e| CodecError::Failed(e.to_string()))?;
        self.run(&tmp, &["-i", &inp.to_string_lossy(), "-f", "rawvideo", "-pix_fmt", pix_fmt(hint), &outp.to_string_lossy()])?;
        let data = std::fs::read(&outp).map_err(|e| CodecError::Failed(e.to_string()))?;
        // Size comes from the hint; a wrong-sized stream surfaces as a length error here.
        Canonical::new(*hint, data).map_err(CodecError::Failed)
    }
}
