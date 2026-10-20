# Desktop status strip

The Web Settings page is the configuration surface; it is not meant to stay open
while working. The desktop strip is the at-a-glance view that remains available
when the browser is minimized.

## What it shows

- Initially: `● Vibe Key · ready` and `VibDock · local`.
- After an action: the same verified/pending/failed wording as the action
  feedback overlay, with the captured target.
- It does not invent device activity and never reports a dispatched shortcut as
  a verified application state.

## Placement

The strip is drawn **inside the taskbar**, in the blank band to the left of the
app buttons:

```text
┌──────────────────────────────────────────────────────────────┐
│ desktop                                                      │
├──────────────────────────────────────────────────────────────┤
│ weather   ┌──────────────────────┐   app buttons   tray/clock │
│           │ ● Vibe Key · ready    │                          │
│           │ VibDock · local       │  ← status strip           │
│           └──────────────────────┘                          │
└──────────────────────────────────────────────────────────────┘
```

The Windows adapter creates a normal layered popup, then reparents it into
`Shell_TrayWnd` with `SetParent`, converts it to `WS_CHILD`, removes
`WS_EX_TOPMOST`, and uses `SetWindowPos(HWND_TOP, SWP_FRAMECHANGED)` so it sits
above the taskbar's XAML content child. It is placed 300 px from the taskbar's
left edge and vertically centred. A short delayed re-assert handles the taskbar
finishing its own layout after startup.

This is not a COM deskband. It is a child window of the classic taskbar host,
which is the practical way to put text in the Windows 11 taskbar without a
signed UIAccess binary or undocumented z-band APIs. If `Shell_TrayWnd` cannot
be found or is vertical, it falls back to the work-area corner above the
taskbar. Explorer restarting destroys the child; restart VibDock to recreate it.

## Enabling

- The normal Windows launcher starts the service with `VIBDOCK_OVERLAY=1` and
  `VIBDOCK_OVERLAY_CORNER=taskbar` when the variables are not already set.
- A direct `vibdock service-start` remains opt-in: set both variables manually.
- Disable with `VIBDOCK_OVERLAY=0`.
- Other placements: `bottom-right`, `bottom-left`, `top-right`, `top-left`.
- An explicit environment value always wins over the launcher defaults.

The service starts the strip on the accept-loop thread; a window failure is
logged and does not stop audio, device or action control. The window is resized
to 340×48 px at 96 DPI and respects the process DPI awareness.

## Verification

- `cargo test -p vibdock --lib feedback_windows::` covers the taskbar placement,
  normal corners, origin offsets, style and queue bounds.
- A live run with `VIBDOCK_OVERLAY=1` creates the `VibDockOverlay` window at
  `300,1328..1376` on a 2560×1440 display with a 48 px taskbar; its painted
  content was captured with `PrintWindow` and shows the ready text.
- The in-app screenshots cannot be used as evidence of the user's desktop; the
  strip is verified as a window and paint surface, not visually against every
  Windows taskbar customization.

## Known limits

- It shows the last completed action, not a live per-button state.
- It is a separate topmost window, so a fullscreen exclusive application may
  cover it, like other desktop overlays.
- A future desktop-status toggle in Settings can replace the environment
  variables; none is implemented yet.




