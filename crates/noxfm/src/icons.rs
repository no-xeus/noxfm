//! Icon lookup against the user's icon theme.
//!
//! libcosmic keeps a global theme that it resets to "Cosmic" from its own
//! config at runtime, which most non-COSMIC desktops don't ship. So we
//! resolve paths ourselves and hand libcosmic absolute paths.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{LazyLock, Mutex};

use cosmic::widget::icon;

static THEME: LazyLock<String> = LazyLock::new(|| {
    candidates().into_iter().find(|t| installed(t)).unwrap_or_else(|| "hicolor".into())
});

type Cache = HashMap<(String, u16), Option<PathBuf>>;
static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(Default::default);

pub fn theme() -> &'static str {
    &THEME
}

/// First of `names` that the theme (or its parents) provides.
pub fn find(names: &[impl AsRef<str>], size: u16) -> Option<PathBuf> {
    names.iter().find_map(|n| {
        let key = (n.as_ref().to_owned(), size);
        CACHE
            .lock()
            .unwrap()
            .entry(key)
            .or_insert_with(|| {
                freedesktop_icons::lookup(n.as_ref()).with_theme(theme()).with_size(size).find()
            })
            .clone()
    })
}

pub fn handle(names: &[impl AsRef<str>], size: u16) -> icon::Handle {
    let first = names.first().map_or("image-missing", |n| n.as_ref());
    match find(names, size) {
        Some(p) => {
            let mut h = icon::from_path(p);
            // Symbolic icons are recolored to match the theme's foreground.
            h.symbolic = first.ends_with("-symbolic");
            h
        }
        None => icon::from_name(first).size(size).handle(),
    }
}

pub fn get(names: &[impl AsRef<str>], size: u16) -> icon::Icon {
    icon::icon(handle(names, size)).size(size)
}

fn candidates() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(t) = std::env::var("NOXFM_ICON_THEME") {
        out.push(t);
    }
    if let Ok(o) = Command::new("gsettings").args(["get", "org.gnome.desktop.interface", "icon-theme"]).output() {
        out.push(String::from_utf8_lossy(&o.stdout).trim().trim_matches('\'').to_owned());
    }
    for f in ["gtk-4.0/settings.ini", "gtk-3.0/settings.ini"] {
        let Ok(s) = std::fs::read_to_string(config_home().join(f)) else { continue };
        if let Some(v) = s.lines().find_map(|l| l.trim().strip_prefix("gtk-icon-theme-name")) {
            out.push(v.trim_start_matches([' ', '=']).trim().to_owned());
        }
    }
    out.extend(["Cosmic", "Adwaita", "breeze-dark", "breeze"].map(String::from));
    out.retain(|t| !t.is_empty());
    out
}

fn installed(theme: &str) -> bool {
    data_dirs().iter().any(|d| d.join("icons").join(theme).join("index.theme").is_file())
}

fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| noxfm_core::complete::home_dir().join(".config"))
}

fn data_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| noxfm_core::complete::home_dir().join(".local/share"));
    let sys = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    std::iter::once(home).chain(sys.split(':').map(PathBuf::from)).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn resolves_common_icons() {
        for name in ["folder", "go-up-symbolic", "text-x-generic"] {
            assert!(super::find(&[name], 24).is_some(), "{name} in {}", super::theme());
        }
        assert_eq!(super::find(&["no-such-icon-xyz", "folder"], 24), super::find(&["folder"], 24));
    }
}
