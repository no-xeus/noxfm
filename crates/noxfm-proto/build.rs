//! Hashes the protocol source so binaries built from different revisions of
//! it refuse each other at handshake instead of failing to decode later.

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
}
