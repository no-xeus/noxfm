# Known issues

## To confirm: drag and drop on Hyprland

Broken with the libcosmic windows (below); not tried yet with the GTK4 ones.

## Gone with the move to GTK4

The first windows were built on libcosmic. Two problems came from it; the
windows moved to GTK4 on 2026-10-09 (see [ui-rewrite.md](ui-rewrite.md)):

- **Crash when opening a context menu** after using one, once the selection
  had changed (one file → several, or after clicking a folder in the
  sidebar). libcosmic kept the id of a menu popup after it was closed, and
  laid the next menu out against stale widgets.
- **Drag and drop did nothing on Hyprland** (Hyprland 0.56.2). The
  compositor negotiated "no action" with the drag-and-drop code libcosmic
  used, so every drop was cancelled. GTK4's drag and drop is a different
  implementation, expected to work. If drops still fail, start the daemon
  with `NOXFM_LOG=debug noxd` and look for `drop … item(s) on …` in
  `~/.local/state/noxfm/windows.log`.
