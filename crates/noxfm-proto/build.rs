//! - Hashes the protocol source, so binaries built from different revisions
//!   of it refuse each other at handshake instead of failing to decode later.
//! - Records the git revision (`git describe`) for `--version`.

fn main() {
    let mut files: Vec<_> = std::fs::read_dir("src").unwrap().map(|e| e.unwrap().path()).collect();
    files.sort();
    // FNV-1a: stable across Rust versions, unlike DefaultHasher.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for f in files {
        println!("cargo:rerun-if-changed={}", f.display());
        for b in std::fs::read(&f).unwrap() {
            h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
        }
    }
    println!("cargo:rustc-env=NOXFM_SCHEMA={h:016x}");

    // e.g. "v0.1.0-3-g1a2b3c4-dirty"; "unknown" outside a git checkout.
    let describe = std::process::Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty", "--abbrev=7"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=NOXFM_GIT={describe}");
    // Rebuild when the commit or the index changes (relative to this crate).
    for f in ["../../.git/HEAD", "../../.git/index", "../../.git/refs/tags"] {
        if std::path::Path::new(f).exists() {
            println!("cargo:rerun-if-changed={f}");
        }
    }
}
