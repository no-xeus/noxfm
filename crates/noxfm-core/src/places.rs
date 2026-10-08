//! The user's folders ("listen directories") and saved mount policies.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use noxfm_proto::{MountPolicy, RecentKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    pub name: String,
    pub path: PathBuf,
    /// Files appearing here count as downloads rather than creations.
    pub downloads: bool,
}

/// XDG user directories (from `user-dirs.dirs`, falling back to the usual
/// names) plus `~/Projects`, keeping only those that exist.
pub fn listen_dirs(home: &Path, user_dirs: Option<&str>) -> Vec<Place> {
    let configured = user_dirs.map(|t| parse_user_dirs(t, home)).unwrap_or_default();
    let dir = |key: &str, fallback: &str| configured.get(key).cloned().unwrap_or_else(|| home.join(fallback));
    let candidates = [
        ("Downloads", dir("DOWNLOAD", "Downloads"), true),
        ("Documents", dir("DOCUMENTS", "Documents"), false),
        ("Projects", home.join("Projects"), false),
        ("Pictures", dir("PICTURES", "Pictures"), false),
        ("Videos", dir("VIDEOS", "Videos"), false),
        ("Music", dir("MUSIC", "Music"), false),
        ("Desktop", dir("DESKTOP", "Desktop"), false),
    ];
    let mut out: Vec<Place> = Vec::new();
    for (name, path, downloads) in candidates {
        // XDG dirs set to $HOME itself mean "disabled".
        if path != home && path.is_dir() && !out.iter().any(|p| p.path == path) {
            out.push(Place { name: name.into(), path, downloads });
        }
    }
    out
}

/// `XDG_DOWNLOAD_DIR="$HOME/Downloads"` -> `{"DOWNLOAD": /home/u/Downloads}`
pub fn parse_user_dirs(text: &str, home: &Path) -> HashMap<String, PathBuf> {
    text.lines()
        .filter_map(|l| {
            let (k, v) = l.trim().split_once('=')?;
            let key = k.strip_prefix("XDG_")?.strip_suffix("_DIR")?;
            let v = v.trim().trim_matches('"');
            let path = match v.strip_prefix("$HOME") {
                Some(rest) => home.join(rest.trim_start_matches('/')),
                None if v.starts_with('/') => PathBuf::from(v),
                None => return None,
            };
            Some((key.to_owned(), path))
        })
        .collect()
}

/// Where "New ▸" templates come from (`XDG_TEMPLATES_DIR`, else ~/Templates).
pub fn templates_dir(home: &Path, user_dirs: Option<&str>) -> PathBuf {
    user_dirs
        .and_then(|t| parse_user_dirs(t, home).remove("TEMPLATES"))
        .filter(|p| p != home)
        .unwrap_or_else(|| home.join("Templates"))
}

/// The kind a new file gets, depending on where it appeared.
pub fn kind_for_new_file(place: &Place) -> RecentKind {
    if place.downloads { RecentKind::Downloaded } else { RecentKind::Created }
}

/// `mounts.conf`: one `<filesystem uuid> <auto|ask|never>` per line.
pub fn parse_policies(text: &str) -> HashMap<String, MountPolicy> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let (uuid, policy) = l.split_once(char::is_whitespace)?;
            let policy = match policy.trim() {
                "auto" => MountPolicy::Auto,
                "ask" => MountPolicy::Ask,
                "never" => MountPolicy::Never,
                _ => return None,
            };
            Some((uuid.to_owned(), policy))
        })
        .collect()
}

pub fn format_policies(policies: &HashMap<String, MountPolicy>) -> String {
    let mut lines: Vec<String> = policies
        .iter()
        .map(|(uuid, p)| {
            let p = match p {
                MountPolicy::Auto => "auto",
                MountPolicy::Ask => "ask",
                MountPolicy::Never => "never",
            };
            format!("{uuid} {p}")
        })
        .collect();
    lines.sort();
    format!("# noxfm mount policies: <filesystem uuid> <auto|ask|never>\n{}\n", lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_dirs() {
        let text = "# comment\nXDG_DOWNLOAD_DIR=\"$HOME/Téléchargements\"\nXDG_MUSIC_DIR=\"/data/music\"\nXDG_DESKTOP_DIR=\"$HOME/\"\nbogus\n";
        let m = parse_user_dirs(text, Path::new("/home/u"));
        assert_eq!(m["DOWNLOAD"], Path::new("/home/u/Téléchargements"));
        assert_eq!(m["MUSIC"], Path::new("/data/music"));
        assert_eq!(m["DESKTOP"], Path::new("/home/u"));
        assert_eq!(m.len(), 3);
    }

    #[test]
    fn listen_dirs_exist_and_skip_home() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path();
        for d in ["Dl", "Projects", "Pictures"] {
            std::fs::create_dir(home.join(d)).unwrap();
        }
        let cfg = "XDG_DOWNLOAD_DIR=\"$HOME/Dl\"\nXDG_DESKTOP_DIR=\"$HOME\"\n";
        let places = listen_dirs(home, Some(cfg));
        let names: Vec<_> = places.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Downloads", "Projects", "Pictures"]);
        assert!(places[0].downloads && places[0].path == home.join("Dl"));
        assert_eq!(kind_for_new_file(&places[1]), RecentKind::Created);
    }

    #[test]
    fn policies_round_trip() {
        let p = parse_policies("# x\n5A94CFBC94CF98C1 never\n14B7-9555   auto\nbad line\nAAAA maybe\n");
        assert_eq!(p.len(), 2);
        assert_eq!(p["5A94CFBC94CF98C1"], MountPolicy::Never);
        assert_eq!(parse_policies(&format_policies(&p)), p);
    }
}
