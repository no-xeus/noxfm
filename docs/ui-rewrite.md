# Window rewrite: libcosmic → GTK4

**Status:** in progress (started 2026-10-09).

## Why

The libcosmic window (`crates/noxfm`) crashes and misbehaves in ways that come
from the toolkit, not from noxfm:

- Context-menu crashes (`index out of bounds` in `button/widget.rs`,
  `Downcast on stateless state`): libcosmic's `close_all` kept a destroyed
  popup's id, so the next menu was laid out against stale widget trees.
  Patched for now in a local fork (see `[patch]` in `Cargo.toml`).
- Drag and drop does nothing on Hyprland (`docs/known-issues.md`).
- Widget state moves between widgets when the view's shape changes (the
  spacers kept in `view()` to avoid that).

GTK4 has mature context menus (popovers), drag and drop, a clipboard that
carries file lists, and list/grid views that only build visible rows.

## What changes, what stays

- **Unchanged:** `noxd`, `noxfm-proto`, `noxfm-core`, packaging layout. The
  window stays a separate process started by noxd, one per window.
- **Shared:** `noxfm_core::fmt` (sizes, dates, icon names) and
  `noxfm_core::preview` (preview loader) moved out of the libcosmic crate.
  `noxfm_core::compare` is the sort order both UIs use.
- **New:** `crates/noxfm-gtk` (binary `noxfm-gtk`), plain gtk4-rs. The daemon
  `Client` runs on a tokio thread; replies and events reach the GTK main loop
  through futures and a channel.
- **Replaced by GTK:** `selection.rs` (GTK selection models), `icons.rs`
  (GTK icon theme), `clipboard.rs` (`gdk::FileList` plus
  `x-special/gnome-copied-files`), libcosmic header bar and theme.
- **Removed at the end:** `crates/noxfm` and every libcosmic dependency.
  Packaging then depends on `gtk4` and no longer builds wgpu.

## Running it during the rewrite

```sh
cargo build
NOXFM_WINDOW_BIN=noxfm-gtk target/debug/noxd   # noxd opens GTK windows
target/debug/noxfm                              # the launcher still asks noxd for a window
```

When the GTK window reaches parity, it takes the binary name `noxfm` and
`NOXFM_WINDOW_BIN` goes away.

## Phases

1. ✅ **Window and listing.** Header bar (back, forward, up), path bar, list view
   (icon, name, type, size, modified) with header sorting, live updates
   (`DirChanged`, `SizeUpdated`, `Moved`), opening files and folders, status
   line, reconnecting to noxd.
2. ✅ **Views.** Icon grid, Ctrl+1/Ctrl+2, zoom (Ctrl + / Ctrl − / Ctrl+wheel),
   keyboard navigation, rubber-band selection, hidden files, created date,
   owner/permissions columns, thumbnails, path completion in a popover above
   the content. Not carried over yet: the default app's icon on file icons
   (FastOpen badge), git badge, mime-mismatch warning — with phase 5 panels.
3. **Actions.** Context menus (items, background, Recent, Trash), clipboard
   with cut hint (cut items dimmed until pasted or replaced), paste and paste
   as link, drag and drop (in, out, onto folders), rename inline, new
   folder/file/template, trash, delete, undo, compress/extract, send to.
4. **Navigation.** Tabs (with detach to a new window), history per tab,
   sidebar (Recent, Places, Pinned, Devices; folding, resizing), Recent and
   Trash views, mounting and mount policies.
5. **Panels.** Properties (files, folders, partitions, disks), preview panel,
   transfers, app picker and other dialogs, health banner.
6. **Switch.** Rename binary, delete `crates/noxfm` and the libcosmic patch,
   update PKGBUILDs, CHANGELOG, `docs/install.md`, `docs/known-issues.md`.

## Feedback to address in the new window

- Cut items keep a visual hint (dimmed) — phase 3.
- Undoing a rename of the folder you're in follows it — done in the daemon
  (`Event::Moved`); the GTK window handles it from phase 1.
- Path completion shows above other elements, not pushing them down — done
  (popover under the path bar).
- Ctrl + / Ctrl − resize items — done (also Ctrl+wheel, Ctrl+0, View menu).
- Read-only files and folders are marked, depending on who runs the window
  (user or root) — to design: lock badge on the icon plus a "You can't modify
  this folder" bar with Paste/New disabled; checked with `access(2)` in the
  window process, so a root window sees root's rights. A root window would
  need its own daemon (another runtime dir); a polkit "open as
  administrator" may fit better.
- Context-menu crashes with multiple selection — fixed in the libcosmic fork;
  gone by construction in GTK.
