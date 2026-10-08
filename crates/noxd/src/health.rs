//! What noxfm relies on outside itself, and what stops working without it.

use std::path::Path;

use noxfm_core::apps::{on_path, terminal_prefix};
use noxfm_proto::HealthCheck;

/// Where the noxfm polkit rule can be installed (package, or by hand).
const POLKIT_RULES: &[&str] =
    &["/usr/share/polkit-1/rules.d/50-noxfm-udisks.rules", "/etc/polkit-1/rules.d/50-noxfm-udisks.rules"];

/// Process names (`/proc/*/comm`, cut at 15 chars) of polkit password agents.
const POLKIT_AGENTS: &[&str] = &[
    "hyprpolkitagent",
    "polkit-gnome-au",
    "polkit-kde-auth",
    "polkit-mate-aut",
    "lxpolkit",
    "lxqt-policykit-",
    "xfce-polkit",
    "mate-polkit",
    "soteria",
    "polkit-agent",
];

/// Below this, watching the user's folders for Recent may run out.
const MIN_INODE_WATCHES: u64 = 8192;

fn check(name: &str, ok: bool, detail: impl Into<String>, feature: &str) -> HealthCheck {
    HealthCheck { name: name.into(), ok, detail: detail.into(), feature: feature.into() }
}

fn running(names: &[&str]) -> Option<String> {
    let procs = std::fs::read_dir("/proc").ok()?;
    procs
        .filter_map(Result::ok)
        .filter_map(|e| std::fs::read_to_string(e.path().join("comm")).ok())
        .map(|c| c.trim().to_owned())
        .find(|c| names.iter().any(|n| c.starts_with(n)))
}

/// Blocking: reads /proc and $PATH.
pub fn run(udisks_connected: bool) -> Vec<HealthCheck> {
    let mut v = Vec::new();

    v.push(check(
        "UDisks2",
        udisks_connected,
        if udisks_connected { "connected" } else { "not reachable on the system bus: install and start udisks2" },
        "Drives and partitions in the sidebar, mounting",
    ));

    let rule = POLKIT_RULES.iter().find(|p| Path::new(p).exists());
    let agent = running(POLKIT_AGENTS);
    v.push(check(
        "Mounting internal disks",
        rule.is_some() || agent.is_some(),
        match (rule, &agent) {
            (Some(r), _) => format!("allowed without a password by {r}"),
            (None, Some(a)) => format!("asks for your password through {a}"),
            (None, None) => "no polkit agent is running and the noxfm polkit rule isn't installed".into(),
        },
        "Mounting internal partitions",
    ));

    let tool = |name: &str, feature: &str, pkg: &str| {
        let ok = on_path(name);
        check(name, ok, if ok { "installed".into() } else { format!("not found: install {pkg}") }, feature)
    };
    v.push(tool("ffmpeg", "Video thumbnails", "ffmpeg"));
    v.push(tool("git", "Git branch and status in Properties", "git"));

    let env = std::env::var("TERMINAL").ok();
    let term = terminal_prefix(on_path, env.as_deref());
    v.push(check(
        "Terminal emulator",
        term.is_some(),
        match &term {
            Some(t) => format!("using {}", t[0]),
            None => "none found: install one (kitty, foot, alacritty, …) or set $TERMINAL".into(),
        },
        "Open in terminal, and terminal apps such as Neovim",
    ));

    let watches = std::fs::read_to_string("/proc/sys/fs/inotify/max_user_watches")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok());
    if let Some(n) = watches {
        v.push(check(
            "inotify watches",
            n >= MIN_INODE_WATCHES,
            format!("limit is {n}"),
            "Recent files in large folder trees",
        ));
    }
    v
}

#[cfg(test)]
mod tests {
    #[test]
    fn runs_and_reports_features() {
        let checks = super::run(false);
        let udisks = checks.iter().find(|c| c.name == "UDisks2").unwrap();
        assert!(!udisks.ok && !udisks.feature.is_empty());
        // `sh` is always there; a terminal may or may not be.
        assert!(checks.iter().all(|c| !c.detail.is_empty()));
    }
}
