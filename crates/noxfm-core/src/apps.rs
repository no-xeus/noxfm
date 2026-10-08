//! Which application opens a file: `.desktop` entries and `mimeapps.list`,
//! per the freedesktop Desktop Entry and MIME Applications Associations specs.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct App {
    /// Desktop file ID, e.g. `org.gnome.Loupe.desktop`.
    pub id: String,
    pub name: String,
    pub icon: Option<String>,
    pub exec: String,
    pub path: PathBuf,
    pub mime_types: Vec<String>,
    /// `Terminal=true`: must run inside a terminal emulator.
    pub terminal: bool,
}

/// Parses the `[Desktop Entry]` group. Hidden, NoDisplay-for-everything and
/// non-application entries yield `None`.
pub fn parse_desktop(id: &str, path: &Path, text: &str) -> Option<App> {
    let mut in_main = false;
    let mut kv: HashMap<&str, &str> = HashMap::new();
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_main = line == "[Desktop Entry]";
            continue;
        }
        if !in_main || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            // Unlocalized keys only (`Name`, not `Name[fr]`).
            kv.entry(k.trim()).or_insert(v.trim());
        }
    }
    if kv.get("Type") != Some(&"Application") || kv.get("Hidden") == Some(&"true") {
        return None;
    }
    Some(App {
        id: id.to_owned(),
        name: kv.get("Name")?.to_string(),
        icon: kv.get("Icon").filter(|i| !i.is_empty()).map(|i| i.to_string()),
        exec: kv.get("Exec")?.to_string(),
        path: path.to_path_buf(),
        mime_types: kv
            .get("MimeType")
            .map(|m| m.split(';').filter(|s| !s.is_empty()).map(str::to_owned).collect())
            .unwrap_or_default(),
        terminal: kv.get("Terminal") == Some(&"true"),
    })
}

/// `[Default Applications]` and `[Added Associations]` of one mimeapps.list.
#[derive(Debug, Default)]
pub struct MimeApps {
    defaults: HashMap<String, Vec<String>>,
    added: HashMap<String, Vec<String>>,
}

pub fn parse_mimeapps(text: &str) -> MimeApps {
    let mut out = MimeApps::default();
    let mut group = "";
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            group = line;
            continue;
        }
        let Some((mime, ids)) = line.split_once('=') else { continue };
        let ids: Vec<String> = ids.split(';').map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect();
        let target = match group {
            "[Default Applications]" => &mut out.defaults,
            "[Added Associations]" => &mut out.added,
            _ => continue,
        };
        target.entry(mime.trim().to_owned()).or_default().extend(ids);
    }
    out
}

#[derive(Debug, Default)]
pub struct AppIndex {
    apps: HashMap<String, App>,
    /// mimeapps.list files, most important first.
    lists: Vec<MimeApps>,
}

impl AppIndex {
    pub fn load() -> AppIndex {
        let (config_dirs, data_dirs) = (xdg_config_dirs(), xdg_data_dirs());
        // Earlier data dirs win when two contain the same desktop ID.
        let mut apps = HashMap::new();
        for dir in data_dirs.iter().rev() {
            scan(&dir.join("applications"), "", &mut apps);
        }
        let list_paths = config_dirs
            .iter()
            .map(|d| d.join("mimeapps.list"))
            .chain(data_dirs.iter().map(|d| d.join("applications/mimeapps.list")));
        let lists = list_paths.filter_map(|p| fs::read_to_string(p).ok()).map(|t| parse_mimeapps(&t)).collect();
        AppIndex { apps, lists }
    }

    pub fn from_parts(apps: Vec<App>, lists: Vec<MimeApps>) -> AppIndex {
        AppIndex { apps: apps.into_iter().map(|a| (a.id.clone(), a)).collect(), lists }
    }

    pub fn get(&self, id: &str) -> Option<&App> {
        self.apps.get(id)
    }

    /// Every installed app, by name.
    pub fn all(&self) -> Vec<&App> {
        let mut v: Vec<&App> = self.apps.values().collect();
        v.sort_by_key(|a| a.name.to_lowercase());
        v
    }

    /// Apps that can open `mime`, best first: the default, other configured
    /// associations, then apps declaring the type (by name).
    pub fn apps_for(&self, mime: &str) -> Vec<&App> {
        let mut ids: Vec<&str> = Vec::new();
        ids.extend(self.default_for(mime).map(|a| a.id.as_str()));
        for l in &self.lists {
            ids.extend(l.defaults.get(mime).into_iter().chain(l.added.get(mime)).flatten().map(String::as_str));
        }
        let mut declared: Vec<&App> = self.apps.values().filter(|a| a.mime_types.iter().any(|m| m == mime)).collect();
        declared.sort_by_key(|a| a.name.to_lowercase());
        ids.extend(declared.iter().map(|a| a.id.as_str()));

        let mut seen = std::collections::HashSet::new();
        ids.into_iter().filter(|id| seen.insert(*id)).filter_map(|id| self.apps.get(id)).collect()
    }

    /// The app that opens `mime`: an explicit default first, then added
    /// associations, then any app declaring the type. `text/*` and other
    /// subtypes fall back to `text/plain`.
    pub fn default_for(&self, mime: &str) -> Option<&App> {
        self.lookup(mime).or_else(|| {
            let fallback = if mime.starts_with("text/") || is_textual(mime) { "text/plain" } else { return None };
            (fallback != mime).then(|| self.lookup(fallback)).flatten()
        })
    }

    fn lookup(&self, mime: &str) -> Option<&App> {
        let installed = |ids: Option<&Vec<String>>| ids.into_iter().flatten().find_map(|id| self.apps.get(id));
        self.lists
            .iter()
            .find_map(|l| installed(l.defaults.get(mime)))
            .or_else(|| self.lists.iter().find_map(|l| installed(l.added.get(mime))))
            .or_else(|| {
                let mut candidates: Vec<&App> =
                    self.apps.values().filter(|a| a.mime_types.iter().any(|m| m == mime)).collect();
                candidates.sort_by(|a, b| a.id.cmp(&b.id));
                candidates.into_iter().next()
            })
    }
}

fn is_textual(mime: &str) -> bool {
    matches!(mime, "application/json" | "application/xml" | "application/x-shellscript" | "application/toml")
        || mime.ends_with("+xml")
        || mime.ends_with("+json")
}

/// Rewrites a mimeapps.list so `id` is the default for `mime`, keeping
/// everything else (other groups, comments, other types) as it was.
pub fn set_default(list_text: &str, mime: &str, id: &str) -> String {
    let entry = format!("{mime}={id};");
    let mut out: Vec<String> = Vec::new();
    let mut in_defaults = false;
    let mut done = false;
    for line in list_text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            if in_defaults && !done {
                out.push(entry.clone());
                done = true;
            }
            in_defaults = t == "[Default Applications]";
            out.push(line.to_owned());
            continue;
        }
        if in_defaults && t.split_once('=').is_some_and(|(k, _)| k.trim() == mime) {
            if !done {
                out.push(entry.clone());
                done = true;
            }
            continue;
        }
        out.push(line.to_owned());
    }
    if !done {
        if !in_defaults {
            if out.last().is_some_and(|l| !l.trim().is_empty()) {
                out.push(String::new());
            }
            out.push("[Default Applications]".into());
        }
        out.push(entry);
    }
    out.join("\n") + "\n"
}

pub fn user_mimeapps_list() -> PathBuf {
    xdg_config_dirs().remove(0).join("mimeapps.list")
}

/// Desktop IDs of nested files join the subdirectory with `-`.
fn scan(dir: &Path, prefix: &str, apps: &mut HashMap<String, App>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.filter_map(Result::ok) {
        let name = e.file_name().to_string_lossy().into_owned();
        let path = e.path();
        if path.is_dir() {
            scan(&path, &format!("{prefix}{name}-"), apps);
        } else if name.ends_with(".desktop") {
            let id = format!("{prefix}{name}");
            if let Some(app) = fs::read_to_string(&path).ok().and_then(|t| parse_desktop(&id, &path, &t)) {
                apps.insert(id, app);
            }
        }
    }
}

/// Builds argv from an `Exec` line for opening `file`.
///
/// Field codes: `%f %F` -> the path, `%u %U` -> its file URI, `%i` -> `--icon <icon>`,
/// `%c` -> the app name, `%k` -> the desktop file, `%%` -> `%`. Deprecated codes are
/// dropped. If the line has no file code, the path is appended.
pub fn exec_argv(app: &App, file: &Path) -> Option<Vec<String>> {
    let mut argv = Vec::new();
    let mut used_file = false;
    for token in split_exec(&app.exec)? {
        match token.as_str() {
            "%f" | "%F" => {
                argv.push(file.to_string_lossy().into_owned());
                used_file = true;
            }
            "%u" | "%U" => {
                argv.push(crate::uri::to_uri(file));
                used_file = true;
            }
            "%i" => {
                if let Some(icon) = &app.icon {
                    argv.extend(["--icon".to_owned(), icon.clone()]);
                }
            }
            "%c" => argv.push(app.name.clone()),
            "%k" => argv.push(app.path.to_string_lossy().into_owned()),
            "%d" | "%D" | "%n" | "%N" | "%v" | "%m" => {}
            _ => argv.push(expand_inline(&token, app)),
        }
    }
    if !used_file {
        argv.push(file.to_string_lossy().into_owned());
    }
    (!argv.is_empty()).then_some(argv)
}

/// Codes embedded inside a larger argument (`--name=%c`, `100%%`).
fn expand_inline(token: &str, app: &App) -> String {
    let mut out = String::new();
    let mut chars = token.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('%') => out.push('%'),
            Some('c') => out.push_str(&app.name),
            Some('k') => out.push_str(&app.path.to_string_lossy()),
            Some(_) | None => {}
        }
    }
    out
}

/// Splits per the spec: whitespace separates, double quotes group, and inside
/// quotes `\"`, `` \` ``, `\$` and `\\` are escapes.
fn split_exec(exec: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut has_token = false;
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                has_token = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => cur.push(chars.next()?),
                        c => cur.push(c),
                    }
                }
            }
            ' ' | '\t' => {
                if has_token {
                    out.push(std::mem::take(&mut cur));
                    has_token = false;
                }
            }
            c => {
                cur.push(c);
                has_token = true;
            }
        }
    }
    if has_token {
        out.push(cur);
    }
    Some(out)
}

/// Command prefix that runs a program in a terminal emulator: the
/// `xdg-terminal-exec` wrapper if installed, then `$TERMINAL`, then the first
/// known terminal found by `installed`.
pub fn terminal_prefix(installed: impl Fn(&str) -> bool, env_terminal: Option<&str>) -> Option<Vec<String>> {
    if installed("xdg-terminal-exec") {
        return Some(vec!["xdg-terminal-exec".into()]);
    }
    if let Some(t) = env_terminal.filter(|t| !t.is_empty()) {
        return Some(vec![t.into(), "-e".into()]);
    }
    const KNOWN: &[&[&str]] = &[
        &["kitty"],
        &["foot"],
        &["alacritty", "-e"],
        &["ghostty", "-e"],
        &["wezterm", "start", "--"],
        &["konsole", "-e"],
        &["gnome-terminal", "--"],
        &["xterm", "-e"],
    ];
    KNOWN.iter().find(|t| installed(t[0])).map(|t| t.iter().map(|s| s.to_string()).collect())
}

/// Whether `program` is an executable on `$PATH`.
pub fn on_path(program: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path)
            .any(|d| fs::metadata(d.join(program)).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
    })
}

fn xdg_config_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::complete::home_dir().join(".config"));
    let sys = std::env::var("XDG_CONFIG_DIRS").unwrap_or_else(|_| "/etc/xdg".into());
    std::iter::once(home).chain(sys.split(':').filter(|s| !s.is_empty()).map(PathBuf::from)).collect()
}

pub fn xdg_data_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::complete::home_dir().join(".local/share"));
    let sys = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    std::iter::once(home).chain(sys.split(':').filter(|s| !s.is_empty()).map(PathBuf::from)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str, exec: &str, mimes: &[&str]) -> App {
        App {
            id: id.into(),
            name: id.trim_end_matches(".desktop").into(),
            icon: Some("icon-name".into()),
            exec: exec.into(),
            path: format!("/apps/{id}").into(),
            mime_types: mimes.iter().map(|s| s.to_string()).collect(),
            terminal: false,
        }
    }

    #[test]
    fn lists_apps_for_a_type() {
        let lists = vec![parse_mimeapps("[Default Applications]\nimage/png=b.desktop\n[Added Associations]\nimage/png=c.desktop;\n")];
        let idx = AppIndex::from_parts(
            vec![
                app("a.desktop", "a", &["image/png"]),
                app("b.desktop", "b", &["image/png"]),
                app("c.desktop", "c", &[]),
                app("z.desktop", "z", &["video/mp4"]),
            ],
            lists,
        );
        let ids: Vec<_> = idx.apps_for("image/png").iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["b.desktop", "c.desktop", "a.desktop"]);
        assert_eq!(idx.all().len(), 4);
    }

    #[test]
    fn edits_mimeapps_list() {
        let text = "[Added Associations]\nimage/png=x.desktop;\n\n[Default Applications]\n# keep me\nimage/png=old.desktop\ntext/plain=nvim.desktop\n";
        let out = set_default(text, "image/png", "new.desktop");
        assert_eq!(out, "[Added Associations]\nimage/png=x.desktop;\n\n[Default Applications]\n# keep me\nimage/png=new.desktop;\ntext/plain=nvim.desktop\n");
        let added = set_default(&out, "video/mp4", "vlc.desktop");
        assert!(added.ends_with("text/plain=nvim.desktop\nvideo/mp4=vlc.desktop;\n"));
        assert_eq!(set_default("", "a/b", "x.desktop"), "[Default Applications]\na/b=x.desktop;\n");
        let parsed = parse_mimeapps(&added);
        assert_eq!(parsed.defaults["image/png"], ["new.desktop"]);
    }

    #[test]
    fn picks_a_terminal() {
        let only = |names: &'static [&'static str]| move |p: &str| names.contains(&p);
        assert_eq!(terminal_prefix(only(&["kitty", "xdg-terminal-exec"]), None).unwrap(), ["xdg-terminal-exec"]);
        assert_eq!(terminal_prefix(only(&["kitty"]), Some("foot")).unwrap(), ["foot", "-e"]);
        assert_eq!(terminal_prefix(only(&["xterm", "alacritty"]), None).unwrap(), ["alacritty", "-e"]);
        assert_eq!(terminal_prefix(only(&[]), Some("")), None);
        assert!(on_path("sh"));
        assert!(!on_path("definitely-not-a-program-xyz"));
    }

    #[test]
    fn parses_desktop_files() {
        let text = "[Desktop Entry]\nType=Application\nName=Image Viewer\nName[fr]=Visionneuse\n\
                    Exec=loupe %U\nIcon=org.gnome.Loupe\nMimeType=image/png;image/jpeg;\n\n\
                    [Desktop Action new]\nName=Other\nExec=other\n";
        let a = parse_desktop("loupe.desktop", Path::new("/x"), text).unwrap();
        assert_eq!(a.name, "Image Viewer");
        assert!(!a.terminal);
        let nvim = "[Desktop Entry]\nType=Application\nName=Neovim\nExec=nvim %F\nTerminal=true\n";
        assert!(parse_desktop("nvim.desktop", Path::new("/x"), nvim).unwrap().terminal);
        assert_eq!(a.exec, "loupe %U");
        assert_eq!(a.mime_types, ["image/png", "image/jpeg"]);
        assert!(parse_desktop("h.desktop", Path::new("/x"), "[Desktop Entry]\nType=Application\nName=H\nExec=h\nHidden=true").is_none());
        assert!(parse_desktop("l.desktop", Path::new("/x"), "[Desktop Entry]\nType=Link\nName=L\nURL=x").is_none());
    }

    #[test]
    fn resolves_defaults_in_order() {
        let user = parse_mimeapps("[Default Applications]\nimage/png=missing.desktop;viewer.desktop\n[Added Associations]\nimage/gif=gimp.desktop;\n");
        let system = parse_mimeapps("[Default Applications]\nimage/png=gimp.desktop\ntext/plain=editor.desktop\n");
        let idx = AppIndex::from_parts(
            vec![
                app("viewer.desktop", "viewer %f", &["image/png"]),
                app("gimp.desktop", "gimp %U", &["image/png", "image/gif"]),
                app("editor.desktop", "editor %F", &[]),
                app("vlc.desktop", "vlc %U", &["video/mp4"]),
            ],
            vec![user, system],
        );
        assert_eq!(idx.default_for("image/png").unwrap().id, "viewer.desktop", "user list wins; missing ids skipped");
        assert_eq!(idx.default_for("image/gif").unwrap().id, "gimp.desktop", "added association");
        assert_eq!(idx.default_for("video/mp4").unwrap().id, "vlc.desktop", "declared MimeType");
        assert_eq!(idx.default_for("text/x-rust").unwrap().id, "editor.desktop", "text falls back to text/plain");
        assert!(idx.default_for("application/x-unknown").is_none());
    }

    #[test]
    fn expands_exec() {
        let f = Path::new("/tmp/a b.png");
        let argv = |exec: &str| exec_argv(&app("v.desktop", exec, &[]), f).unwrap();
        assert_eq!(argv("viewer %f"), ["viewer", "/tmp/a b.png"]);
        assert_eq!(argv("viewer --new %U"), ["viewer", "--new", "file:///tmp/a%20b.png"]);
        assert_eq!(argv("viewer"), ["viewer", "/tmp/a b.png"], "no code: appended");
        assert_eq!(argv("env FOO=1 viewer %i %f"), ["env", "FOO=1", "viewer", "--icon", "icon-name", "/tmp/a b.png"]);
        assert_eq!(argv(r#""/opt/My App/run" --title="%c \"x\"" %F"#), ["/opt/My App/run", "--title=v \"x\"", "/tmp/a b.png"]);
        assert_eq!(argv("viewer --zoom=100%% %f"), ["viewer", "--zoom=100%", "/tmp/a b.png"]);
    }

    /// `cargo test -p noxfm-core system_defaults -- --ignored --nocapture`
    #[test]
    #[ignore = "reads the real system configuration"]
    fn system_defaults() {
        let idx = AppIndex::load();
        for mime in ["image/png", "video/mp4", "text/plain", "text/x-rust", "application/pdf", "inode/directory"] {
            let app = idx.default_for(mime);
            eprintln!("{mime:20} -> {:?}", app.map(|a| (&a.id, &a.icon, &a.exec)));
        }
    }
}
