# Installing noxfm

noxfm is two programs:
- `noxd`, a daemon. It watches folders, computes sizes, keeps the Recent log,
  mounts drives and runs transfers.
- `noxfm`, the windows. Running `noxfm [folder]` asks the daemon for a
  window, and starts the daemon first if it isn't running.

## Arch Linux

```sh
cd dist/arch
makepkg -si
systemctl --user enable --now noxd          # start the daemon with your session
xdg-mime default dev.noxfm.Browser.desktop inode/directory   # optional: default file manager
```

`makepkg -si` installs:

| File | Purpose |
|---|---|
| `/usr/bin/noxd`, `/usr/bin/noxfm` | the daemon and the window |
| `/usr/lib/systemd/user/noxd.service` | starts the daemon with your session |
| `/usr/share/applications/dev.noxfm.Browser.desktop` | app launcher; registered for folders |
| `/usr/share/polkit-1/rules.d/50-noxfm-udisks.rules` | mount permission (below) |

## Dependencies

| Needed for | Package | Without it |
|---|---|---|
| Drives and partitions, mounting | `udisks2` (+ `polkit`) | no Devices section |
| Windows | `gtk4` | no window |
| Video thumbnails | `ffmpeg` | videos show a plain icon |
| Git branch/status in Properties | `git` | no Git line |
| "Open in terminal", terminal apps like Neovim | any terminal emulator (`$TERMINAL`, `xdg-terminal-exec`, kitty, foot, alacritty, …) | those actions fail with a message |
| Password prompts for mounting | a polkit agent (e.g. `hyprpolkitagent`) | only what the noxfm rule allows can be mounted |

Each window checks these when it connects. Anything missing is listed in a
"Some features are unavailable" banner, and the daemon logs the same at
startup (`journalctl --user -u noxd`).

## Mount permission (polkit)

udisks already lets you mount plugged-in drives. Internal disks, such as a
Windows partition or a second internal SSD, normally need an administrator
password. The shipped rule lets **local, active members of `wheel`** mount
and unmount them without one:

```js
org.freedesktop.udisks2.filesystem-mount-system
org.freedesktop.udisks2.filesystem-unmount-others
```

To keep the password prompt, delete the rule file. Without the package,
install the rule by hand:

```sh
sudo install -Dm644 dist/polkit/50-noxfm-udisks.rules /etc/polkit-1/rules.d/50-noxfm-udisks.rules
```

Whether an internal partition is mounted automatically, after asking, or never
is noxfm's own setting: right-click the partition in the sidebar. It's stored
in `~/.config/noxfm/mounts.conf`.

## Files noxfm keeps

| File | What |
|---|---|
| `~/.config/noxfm/mounts.conf` | per-partition mount rule |
| `~/.config/noxfm/pins` | folders pinned to the sidebar |
| `~/.config/noxfm/sidebar-collapsed` | folded sidebar sections and disks |
| `~/.config/noxfm/noxfm.conf` | `trash_days = 30`: how long the Trash keeps things |
| `~/.local/state/noxfm/recent.log` | the Recent list |
| `~/.local/state/noxfm/windows.log` | window errors (set `NOXFM_LOG=debug` on the daemon for more) |
| `~/.cache/thumbnails/` | thumbnails, shared with other apps |

## Updating

Rebuild and reinstall the package. From this checkout:

```sh
git pull && cd dist/arch && makepkg -si
```

Once published, update `noxfm-git` from the AUR with your AUR helper.
Package versions come from git (`0.1.0.r3.g1a2b3c4` = 3 commits after
v0.1.0), so every build installs as an upgrade.

You don't need to restart anything:
- a running `noxd` notices its program was replaced and restarts itself into
  the new one, once running transfers have finished;
- open windows reconnect within a second;
- newly opened windows are the new version.

`noxd --version` and `noxfm --version` show the exact build.

## Troubleshooting

- **"noxd was built from a different protocol revision"**: the daemon is
  older than the windows. It restarts into the new version within a few
  seconds by itself. If it was started from a program that has since moved,
  run `systemctl --user restart noxd`.
- **Windows don't appear when the daemon was started by systemd**: windows
  use the display of the `noxfm` command that asked for them. Run `noxfm`
  from your session (a launcher or terminal), not over SSH.
- **Something looks or behaves wrong in a window**: see
  [known-issues.md](known-issues.md), and the window log
  (`~/.local/state/noxfm/windows.log`; start the daemon with
  `NOXFM_LOG=debug noxd` for more).
