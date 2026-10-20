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

The Windows adapter (`src/feedback_windows.rs`) places the `taskbar` corner just
above the work-area bottom edge, offset 300 px from the left:

```text
┌──────────────────────────────────────────────────────────────┐
│ desktop                                                      │
│                                                              │
│   ┌──────────────────────────┐                               │
│   │ ● Vibe Key · ready        │  ← status strip               │
│   │ VibDock · local           │                               │
│   └──────────────────────────┘                               │
├──────────────────────────────────────────────────────────────┤
│ taskbar: weather   blank band   app buttons   tray / clock   │
└──────────────────────────────────────────────────────────────┘
```

The work area excludes the taskbar, so the strip lands immediately above it and
does not cover Start, app buttons or the notification area. It is a topmost,
layered, mouse-transparent tool window: it does not take focus or swallow clicks.

This is **not** a Windows deskband or a taskbar-internal widget. Windows 11's
XAML taskbar does not provide a stable public child-window slot; an attempt to
reparent into `Shell_TrayWnd` can be painted behind the XAML surface. The strip
therefore sits directly above the taskbar, where it remains visible with the
normal taskbar layout.

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
