# Window rewrite: libcosmic → GTK4

**Status:** done (2026-10-09). The GTK4 window is `crates/noxfm`; libcosmic
and its patched clone are no longer used.

## Why

The libcosmic window (`crates/noxfm`) crashes and misbehaves in ways that come
from the toolkit, not from noxfm:

- Context-menu crashes (`index out of bounds` in `button/widget.rs`,
  `Downcast on stateless state`): libcosmic's `close_all` kept a destroyed
  popup's id, so the next menu was laid out against stale widget trees.
  It was patched in a local fork while the rewrite went on.
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
- **New:** `crates/noxfm` (was `crates/noxfm-gtk` during the rewrite), plain gtk4-rs. The daemon
  `Client` runs on a tokio thread; replies and events reach the GTK main loop
  through futures and a channel.
- **Replaced by GTK:** `selection.rs` (GTK selection models), `icons.rs`
  (GTK icon theme), `clipboard.rs` (`gdk::FileList` plus
  `x-special/gnome-copied-files`), libcosmic header bar and theme.
- **Removed:** the libcosmic window and every libcosmic dependency, with
  the `[patch]` pointing at the local clone. Packaging depends on `gtk4` and
  no longer builds wgpu.

## Tests

`cargo test -p noxfm` drives a real window against a headless noxd on
an offscreen display (`gtk4-broadwayd`, from the gtk4 package; skipped when
missing). Input can't be injected there, so it calls what keys and menus
call. Drag and drop, and how things look, still need a manual check.

## Phases

1. ✅ **Window and listing.** Header bar (back, forward, up), path bar, list view
   (icon, name, type, size, modified) with header sorting, live updates
   (`DirChanged`, `SizeUpdated`, `Moved`), opening files and folders, status
   line, reconnecting to noxd.
2. ✅ **Views.** Icon grid, Ctrl+1/Ctrl+2, zoom (Ctrl + / Ctrl − / Ctrl+wheel),
   keyboard navigation, rubber-band selection, hidden files, created date,
   owner/permissions columns, thumbnails, path completion in a popover above
   the content.
3. ✅ **Actions.** Context menus (items, background), clipboard with cut
   hint (cut items dimmed until pasted or replaced), paste and paste as link,
   drag and drop (in, out, onto folders), rename in place, new
   folder/file/template, trash, delete, undo, compress/extract, send to.
   Recent and Trash menus come with those views (phase 4); "Open with ▸
   Other application…" and Properties with phase 5.
4. ✅ **Navigation.** Tabs (reorder by dragging; dragged out of the bar, a
   tab opens in its own window; middle click on a folder opens a tab, on a
   tab closes it), history per tab, sidebar (Recent, Places, Pinned,
   disks; folding shared through noxd, resizable, F9), Recent (newest first)
   and Trash views with their menus, mounting, mount rules, "mount this?"
   banners. Disk and partition Properties come with phase 5.
5. ✅ **Panels.** Properties in small windows of their own (files and
   folders with sizes, dates, owner, permission check boxes, default app
   and "Change…", git; several items; partitions; disks), preview panel
   (Space; images whole, text and hex), transfers (indicator in the status
   line, list with Cancel/Dismiss), app picker, health banner, badges
   (default app on file icons, "git", content/extension mismatch).
6. ✅ **Switch.** Rename binary, delete `crates/noxfm` and the libcosmic patch,
   update PKGBUILDs, CHANGELOG, `docs/install.md`, `docs/known-issues.md`.

## Feedback to address in the new window

- Cut items keep a visual hint (dimmed) — done.
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
- Context-menu crashes with multiple selection — gone with libcosmic.
