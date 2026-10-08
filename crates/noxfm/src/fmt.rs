use chrono::{Local, TimeZone};
use noxfm_proto::{Entry, EntryKind, Timestamp};

pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    format!("{v:.1} {}", UNITS[u])
}

pub fn time(ts: Option<Timestamp>) -> String {
    match ts.and_then(|t| Local.timestamp_opt(t, 0).single()) {
        Some(t) => t.format("%Y-%m-%d %H:%M").to_string(),
        None => "—".into(),
    }
}

/// "just now", "5 min ago", "3 h ago", "2 d ago", then the date.
pub fn ago(ts: Timestamp) -> String {
    ago_from(ts, Local::now().timestamp())
}

fn ago_from(ts: Timestamp, now: Timestamp) -> String {
    match now - ts {
        ..60 => "just now".into(),
        s @ 60..3600 => format!("{} min ago", s / 60),
        s @ 3600..86_400 => format!("{} h ago", s / 3600),
        s @ 86_400..604_800 => format!("{} d ago", s / 86_400),
        _ => time(Some(ts)),
    }
}

/// `/home/u/Projects/x` -> `~/Projects/x`
pub fn short_path(p: &std::path::Path) -> String {
    let home = noxfm_core::complete::home_dir();
    match p.strip_prefix(&home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}

/// Keeps the end of `s`, which is the informative part of a path:
/// `…/crates/noxfm/src`.
pub fn ellipsize_start(s: &str, max_chars: usize) -> String {
    let n = s.chars().count();
    if n <= max_chars {
        return s.to_owned();
    }
    let tail: String = s.chars().skip(n - (max_chars - 1)).collect();
    format!("…{tail}")
}

/// `75` -> `"1m 15s"`
pub fn duration(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m {}s", secs / 60, secs % 60),
        _ => format!("{}h {}m", secs / 3600, secs % 3600 / 60),
    }
}

/// Freedesktop icon names to try, most specific first.
pub fn icon_names(e: &Entry) -> Vec<String> {
    match e.kind {
        EntryKind::Dir => vec!["folder".into()],
        EntryKind::BrokenLink => vec!["emblem-unreadable".into(), "text-x-generic".into()],
        EntryKind::Other => vec!["application-x-executable".into(), "text-x-generic".into()],
        EntryKind::File => {
            let mime = e.mime.as_deref().unwrap_or("application/octet-stream");
            let top = mime.split('/').next().unwrap_or("application");
            vec![
                mime.replace('/', "-"),
                format!("{top}-x-generic"),
                "text-x-generic".into(),
            ]
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn sizes() {
        assert_eq!(super::size(0), "0 B");
        assert_eq!(super::size(1536), "1.5 KiB");
        assert_eq!(super::size(5 * 1024 * 1024 * 1024), "5.0 GiB");
        assert_eq!(super::duration(75), "1m 15s");
        assert_eq!(super::duration(7300), "2h 1m");
        assert_eq!(super::ago_from(1000, 1030), "just now");
        assert_eq!(super::ellipsize_start("~/a/b", 10), "~/a/b");
        assert_eq!(super::ellipsize_start("~/Projects/noxfm/src", 10), "…noxfm/src");
        assert_eq!(super::ago_from(0, 300), "5 min ago");
        assert_eq!(super::ago_from(0, 7200), "2 h ago");
        assert_eq!(super::ago_from(0, 3 * 86_400), "3 d ago");
    }
}
