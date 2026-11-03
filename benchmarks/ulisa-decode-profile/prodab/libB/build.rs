// Captures build-time facts (target triple, profile, key dependency versions) so benchmark
// results can say exactly what produced them.
use std::{env, fs};

fn lock_version(lock: &str, pkg: &str) -> String {
    let needle = format!("name = \"{pkg}\"\nversion = \"");
    lock.find(&needle)
        .and_then(|i| {
            let rest = &lock[i + needle.len()..];
            rest.find('"').map(|j| rest[..j].to_string())
        })
        .unwrap_or_else(|| "unknown".into())
}

fn main() {
    println!("cargo:rerun-if-changed=Cargo.lock");
    println!("cargo:rustc-env=NS_TARGET={}", env::var("TARGET").unwrap_or_default());
    println!("cargo:rustc-env=NS_PROFILE={}", env::var("PROFILE").unwrap_or_default());
    println!("cargo:rustc-env=NS_OPT_LEVEL={}", env::var("OPT_LEVEL").unwrap_or_default());
    let lock = fs::read_to_string("Cargo.lock").unwrap_or_default().replace("\r\n", "\n");
    println!("cargo:rustc-env=NS_IMAGE_VERSION={}", lock_version(&lock, "image"));
    println!("cargo:rustc-env=NS_PNG_VERSION={}", lock_version(&lock, "png"));
}
