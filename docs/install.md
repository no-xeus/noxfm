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
| Rendering | `wayland`, `libxkbcommon`, `vulkan-icd-loader` | no window (software rendering is used without Vulkan) |
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

## Troubleshooting

- **"noxd was built from a different protocol revision"**: the daemon is
  older than the windows. Run `systemctl --user restart noxd`.
- **Windows don't appear when the daemon was started by systemd**: windows
  use the display of the `noxfm` command that asked for them. Run `noxfm`
  from your session (a launcher or terminal), not over SSH.
- **Drag and drop between windows does nothing on Hyprland**: see
  [known-issues.md](known-issues.md). Use copy/paste instead.
