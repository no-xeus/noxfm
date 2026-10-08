//! Path-bar autocompletion.

use std::path::{Path, PathBuf};

const MAX_RESULTS: usize = 50;

/// Completes the last component of `input` against directories on disk.
/// `~` expands to `home`. Results are full paths ending in `/`.
/// Hidden dirs are only offered when the typed fragment starts with `.`.
pub fn complete_dirs(input: &str, home: &Path) -> Vec<String> {
    let expanded = expand_tilde(input, home);
    let (parent, frag) = match expanded.rfind('/') {
        Some(i) => (&expanded[..=i], &expanded[i + 1..]),
        None => return Vec::new(),
    };
    let Ok(rd) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    let frag_lc = frag.to_lowercase();
    let mut out: Vec<String> = rd
        .filter_map(Result::ok)
        .filter(|d| d.path().is_dir())
        .filter_map(|d| d.file_name().into_string().ok())
        .filter(|n| frag.starts_with('.') || !n.starts_with('.'))
        .filter(|n| n.to_lowercase().starts_with(&frag_lc))
        .map(|n| format!("{parent}{n}/"))
        .collect();
    out.sort_by_key(|s| s.to_lowercase());
    out.truncate(MAX_RESULTS);
    out
}

pub fn expand_tilde(input: &str, home: &Path) -> String {
    match input.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            format!("{}{rest}", home.display())
        }
        _ => input.to_owned(),
    }
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completes_case_insensitively() {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path();
        for d in ["Projects", "pictures", ".config", "Public"] {
            std::fs::create_dir(r.join(d)).unwrap();
        }
        std::fs::write(r.join("plain.txt"), "").unwrap();
        let base = r.display().to_string();

        let got = complete_dirs(&format!("{base}/p"), Path::new("/"));
        assert_eq!(got, [format!("{base}/pictures/"), format!("{base}/Projects/"), format!("{base}/Public/")]);

        assert_eq!(complete_dirs(&format!("{base}/.c"), Path::new("/")), [format!("{base}/.config/")]);
        assert_eq!(complete_dirs("~/Pro", r), [format!("{base}/Projects/")]);
    }
}
