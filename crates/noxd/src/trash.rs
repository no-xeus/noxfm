//! The freedesktop Trash, shared with other file managers (`~/.local/share/Trash`,
//! plus `.Trash-$uid` on other volumes), via the `trash` crate.
//!
//! - "Delete" moves things here.
//! - Anything older than `trash_days` (default 30) is purged automatically,
//!   at startup and every few hours.

use std::path::{Path, PathBuf};
use std::time::Duration;

use noxfm_proto::{Timestamp, TrashEntry};
use trash::TrashItem;
use trash::os_limited;

/// How often the purge of old items runs.
pub const PURGE_EVERY: Duration = Duration::from_secs(6 * 3600);
pub const DEFAULT_DAYS: u32 = 30;

pub fn now() -> Timestamp {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// `trash_days = N` from `~/.config/noxfm/noxfm.conf`.
pub fn configured_days(config_text: Option<&str>) -> u32 {
    config_text
        .into_iter()
        .flat_map(str::lines)
        .filter_map(|l| l.split_once('='))
        .find(|(k, _)| k.trim() == "trash_days")
        .and_then(|(_, v)| v.trim().parse().ok())
        .unwrap_or(DEFAULT_DAYS)
}

fn entry(item: &TrashItem) -> TrashEntry {
    let file = file_in_trash(item);
    let meta = std::fs::symlink_metadata(&file).ok();
    TrashEntry {
        id: item.id.to_string_lossy().into_owned(),
        name: item.name.to_string_lossy().into_owned(),
        original_path: item.original_path(),
        deleted_at: item.time_deleted,
        purge_at: None,
        size: meta.as_ref().filter(|m| m.is_file()).map(|m| m.len()),
        is_dir: meta.is_some_and(|m| m.is_dir()),
    }
}

/// `<trash>/info/x.trashinfo` (the item id) -> `<trash>/files/x`.
fn file_in_trash(item: &TrashItem) -> PathBuf {
    let info = Path::new(&item.id);
    let trash_dir = info.parent().and_then(Path::parent).unwrap_or(Path::new("/"));
    let stem = info.file_stem().unwrap_or_default();
    trash_dir.join("files").join(stem)
}

fn list_items() -> anyhow::Result<Vec<TrashItem>> {
    Ok(os_limited::list()?)
}

/// Newest first, each with the trashed file's details under its original name.
pub fn list() -> anyhow::Result<Vec<(TrashEntry, noxfm_proto::Entry)>> {
    let mut v: Vec<(TrashEntry, noxfm_proto::Entry)> = list_items()?
        .iter()
        .filter_map(|i| {
            let mut e = noxfm_core::stat_entry(&file_in_trash(i)).ok()?;
            e.name = i.name.to_string_lossy().into_owned();
            e.hidden = false;
            Some((entry(i), e))
        })
        .collect();
    v.sort_by(|(a, _), (b, _)| b.deleted_at.cmp(&a.deleted_at).then_with(|| a.name.cmp(&b.name)));
    Ok(v)
}

pub fn count() -> u32 {
    list_items().map_or(0, |v| v.len() as u32)
}

/// Moves `paths` to the trash. Returns their trash ids (for undo).
pub fn trash(paths: &[PathBuf]) -> anyhow::Result<Vec<String>> {
    let started = now();
    trash::delete_all(paths)?;
    // The trash crate doesn't say where things went: find them again.
    let items = list_items()?;
    Ok(paths
        .iter()
        .filter_map(|p| {
            items
                .iter()
                .filter(|i| i.original_path() == *p && i.time_deleted >= started - 1)
                .max_by_key(|i| i.time_deleted)
                .map(|i| i.id.to_string_lossy().into_owned())
        })
        .collect())
}

fn by_ids(ids: &[String]) -> anyhow::Result<Vec<TrashItem>> {
    let items: Vec<TrashItem> = list_items()?.into_iter().filter(|i| ids.iter().any(|id| *id == *i.id.to_string_lossy())).collect();
    anyhow::ensure!(!items.is_empty() || ids.is_empty(), "those items are no longer in the trash");
    Ok(items)
}

/// Puts items back where they were. If something now has that name, the
/// restored item becomes `name (1)`. Returns where each one went.
pub fn restore(ids: &[String]) -> anyhow::Result<Vec<PathBuf>> {
    let mut restored = Vec::new();
    for item in by_ids(ids)? {
        let original = item.original_path();
        match os_limited::restore_all([item.clone()]) {
            Ok(()) => restored.push(original),
            Err(trash::Error::RestoreCollision { .. }) => {
                std::fs::create_dir_all(&item.original_parent)?;
                let target = noxfm_core::transfer::unique_target(&item.original_parent, Path::new(&item.name));
                // Same filesystem: the trash lives on the item's volume.
                std::fs::rename(file_in_trash(&item), &target)?;
                std::fs::remove_file(Path::new(&item.id))?;
                restored.push(target);
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(restored)
}

pub fn purge(ids: &[String]) -> anyhow::Result<()> {
    os_limited::purge_all(by_ids(ids)?)?;
    Ok(())
}

pub fn empty() -> anyhow::Result<()> {
    os_limited::purge_all(list_items()?)?;
    Ok(())
}

/// Deletes items trashed more than `days` days before `now`. Returns how many.
pub fn purge_older_than(days: u32, now: Timestamp) -> anyhow::Result<usize> {
    let cutoff = now - i64::from(days) * 86_400;
    let old: Vec<TrashItem> = list_items()?.into_iter().filter(|i| i.time_deleted < cutoff).collect();
    let n = old.len();
    if n > 0 {
        os_limited::purge_all(old)?;
    }
    Ok(n)
}

/// Deletes for good, no trash involved.
pub fn delete_forever(paths: &[PathBuf]) -> anyhow::Result<()> {
    for p in paths {
        let meta = std::fs::symlink_metadata(p)?;
        if meta.is_dir() { std::fs::remove_dir_all(p) } else { std::fs::remove_file(p) }
            .map_err(|e| anyhow::anyhow!("{}: {e}", p.display()))?;
    }
    Ok(())
}

/// The home trash's `files/` folder, watched for changes made by other apps.
pub fn home_trash_files() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| noxfm_core::complete::home_dir().join(".local/share"))
        .join("Trash/files")
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_days() {
        assert_eq!(super::configured_days(None), 30);
        assert_eq!(super::configured_days(Some("# x\ntrash_days = 7\n")), 7);
        assert_eq!(super::configured_days(Some("trash_days=nope")), 30);
    }
}
