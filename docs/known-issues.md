# Known issues

## Drag and drop does nothing on Hyprland

**Status:** open. Parked on 2026-10-08 so other POC work could continue.
**Seen with:** Hyprland 0.56.2, libcosmic `60ad2cc`.

You can start a drag and the target window receives the offer. When you drop, nothing gets transferred.

From the trace log, one drag looks like this:

```
Offer Enter { mime_types: ["text/uri-list"] }
Offer SelectedAction(DndAction(0x0))      <- the compositor negotiates "no action"
Offer Enter { mime_types: [] }
Offer Drop
Source Cancelled                          <- so the source is cancelled and no data is read
```

The source offers `Copy | Move` and every drop target accepts `Copy | Move` with
`Move` preferred, yet Hyprland always reports action `0`. Per the
`wl_data_device` protocol, a drop with no negotiated action cancels the source.
So this looks like an action-negotiation incompatibility between Hyprland and the
smithay-clipboard drag-and-drop code that libcosmic uses, not a noxfm bug.

Ideas for later:
- Check whether cosmic-files (same toolkit) shows the same behaviour on Hyprland.
- Try a source offering only one action (`Copy`).
- On a drop with action `0`, read the offer anyway and decide copy or move
  ourselves. This depends on whether the offer can still be read before the
  cancel.

To collect a trace, start the daemon with
`NOXFM_LOG="warn,noxfm=debug,libcosmic::widget::dnd_destination=trace,iced::winit::clipboard=trace" noxd`
and read `~/.local/state/noxfm/windows.log`.

Copy/cut/paste through the clipboard (Ctrl+C / Ctrl+X / Ctrl+V) works and covers the same use case for now.
