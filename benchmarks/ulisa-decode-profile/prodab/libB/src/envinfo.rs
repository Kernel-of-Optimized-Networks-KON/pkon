//! Environment facts that make a result interpretable.
use serde::Serialize;
use std::process::Command;
use std::time::Instant;

#[derive(Serialize, Clone)]
pub struct EnvInfo {
    pub nanoscope_version: &'static str,
    pub git_describe: String,
    pub rustc: String,
    pub target: &'static str,
    pub profile: &'static str,
    pub opt_level: &'static str,
    pub debug_assertions: bool,
    /// Compile-time target features actually enabled (portable baseline unless RUSTFLAGS changed it).
    pub compiled_target_features: Vec<&'static str>,
    pub os: String,
    pub cpu: String,
    pub logical_cpus: usize,
    /// Features the CPU reports at runtime (not necessarily used by this build).
    pub cpu_runtime_features: Vec<&'static str>,
    pub ffmpeg: Option<String>,
    /// Median wall time of `ffmpeg -version`: the floor on every ffmpeg-based timing.
    pub ffmpeg_spawn_ms: Option<f64>,
}

fn cmd_line(prog: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(prog).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).lines().find(|l| !l.trim().is_empty()).map(|l| l.trim().to_string())
}

#[cfg(target_arch = "x86_64")]
fn runtime_features() -> Vec<&'static str> {
    let mut v = vec![];
    macro_rules! chk { ($($f:tt),*) => { $( if std::is_x86_feature_detected!($f) { v.push($f); } )* } }
    chk!("sse2", "sse4.2", "popcnt", "avx", "avx2", "bmi1", "bmi2", "fma", "avx512f", "avx512bw", "avx512vl");
    v
}
#[cfg(not(target_arch = "x86_64"))]
fn runtime_features() -> Vec<&'static str> {
    vec![]
}

fn compiled_features() -> Vec<&'static str> {
    let mut v = vec![];
    macro_rules! chk { ($($f:tt),*) => { $( if cfg!(target_feature = $f) { v.push($f); } )* } }
    chk!("sse2", "sse4.2", "popcnt", "avx", "avx2", "bmi2", "fma", "avx512f", "neon");
    v
}

fn spawn_ms(exe: &str) -> Option<f64> {
    let mut t: Vec<f64> = (0..5)
        .filter_map(|_| {
            let s = Instant::now();
            Command::new(exe).arg("-version").output().ok()?;
            Some(s.elapsed().as_secs_f64() * 1000.0)
        })
        .collect();
    t.sort_by(|a, b| a.partial_cmp(b).unwrap());
    t.get(t.len() / 2).copied()
}

pub fn collect() -> EnvInfo {
    let ff = crate::ext::ffmpeg_version();
    let os = if cfg!(windows) {
        cmd_line("cmd", &["/C", "ver"]).unwrap_or_default()
    } else {
        cmd_line("uname", &["-sr"]).unwrap_or_default()
    };
    EnvInfo {
        nanoscope_version: env!("CARGO_PKG_VERSION"),
        git_describe: cmd_line("git", &["describe", "--always", "--dirty", "--broken"])
            .unwrap_or_else(|| "no commits yet".into()),
        rustc: cmd_line("rustc", &["-V"]).unwrap_or_else(|| "unknown".into()),
        target: env!("NS_TARGET"),
        profile: env!("NS_PROFILE"),
        opt_level: env!("NS_OPT_LEVEL"),
        debug_assertions: cfg!(debug_assertions),
        compiled_target_features: compiled_features(),
        os: format!("{} {}", std::env::consts::OS, os).trim().to_string(),
        cpu: std::env::var("PROCESSOR_IDENTIFIER").unwrap_or_else(|_| "unknown".into()),
        logical_cpus: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
        cpu_runtime_features: runtime_features(),
        ffmpeg_spawn_ms: if ff.is_some() { spawn_ms(&crate::ext::exe()) } else { None },
        ffmpeg: ff,
    }
}
