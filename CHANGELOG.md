# Changelog

All notable changes. Versions follow [semantic versioning](https://semver.org):
- **patch** (0.1.x): fixes;
- **minor** (0.x.0): new features (before 1.0, also breaking changes, such as
  a protocol change between noxd and windows).

## [Unreleased]

## [0.1.0] - 2026-10-09

First proof of concept.

### Daemon (`noxd`)
- Directory listing over a Unix socket. Folders are watched live, and their
  recursive sizes are computed in the background.
- Recent: files downloaded, edited or created in your user folders.
- Drives and partitions through UDisks2, with a per-partition rule: mount
  automatically, ask, or never.
- Copy, move, compress (.zip) and extract as jobs with progress and cancel.
  Copies use reflinks on btrfs/xfs.
- Trash shared with other apps (freedesktop), with automatic purge after
  30 days (`trash_days`).
- Undo for rename, move, copy, trash, new items and links.
- Thumbnails (images, first frame of videos) in the shared cache.
- Default applications from `mimeapps.list`; terminal apps open in a terminal.
- Dependency check at startup.
- Restarts itself into a newly installed version.

### Windows (`noxfm`)
- Tabs with back/forward history. A tab dragged out of the bar (or "Move to
  new window") becomes its own window.
- List and icon views, rubber-band selection, keyboard navigation,
  clipboard compatible with other file managers.
- Windows 10-style context menus.
- Floating Properties panels for files, folders, partitions and disks.
- Preview panel for files without a thumbnail.
- Sidebar: Recent, Places, Pinned, Devices. Sections fold; the sidebar can
  be resized and hidden.
- Windows opened from a window inherit its sidebar layout.

### Packaging
- Arch PKGBUILD (from the checkout) and an AUR `noxfm-git` PKGBUILD.
- systemd user unit, desktop file, polkit rule for mounting internal disks.

### Known issues
- Drag and drop between windows does nothing on Hyprland
  ([docs/known-issues.md](docs/known-issues.md)).

[Unreleased]: https://github.com/no-xeus/noxfm/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/no-xeus/noxfm/releases/tag/v0.1.0
