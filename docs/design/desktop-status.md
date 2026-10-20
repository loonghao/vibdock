# Desktop status

The Web Settings page is for configuration; it is not meant to stay open while
working. The primary notification-area icon now doubles as the persistent status
indicator.

## What it shows

| Ring | Meaning |
| --- | --- |
| green | `Vibe Key · 就绪`, or the latest action completed |
| amber | a recent action was dispatched / completed |
| red | device unavailable, or the latest action failed |
| grey | service not reachable yet |

The tooltip contains the latest action text (for example `Button 2 · DONE`).
When the text changes, a short balloon appears near the notification area.
Clicking the icon opens Settings.

## Why the tray icon

Windows 11's taskbar is a XAML surface. A normal application window — even one
reparented into `Shell_TrayWnd` or its XAML content host — can appear in a
`PrintWindow` capture while the real desktop composition still paints it behind
the taskbar. This was tested with a topmost popup, a taskbar child, a XAML-host
child and an external z-order helper; the strip was not reliably visible on the
real desktop.

A notification-area icon is part of the supported Windows shell contract, so it
is visible inside the taskbar without signing, UIAccess, a COM deskband, or
undocumented z-band APIs.

## Status source

The tray worker already polls the authenticated service. Each poll now also
fetches `GET /api/hardware` and derives:

- `connected`;
- the last `recent_actions` entry (`label`, `state`, `error`).

The primary tray icon is updated with one of the generated ring icons:

- `assets/brand/status-ready.ico`
- `assets/brand/status-active.ico`
- `assets/brand/status-error.ico`
- `assets/brand/status-unknown.ico`

If an icon file is unavailable, the tray falls back to the existing brand icon.
The tooltip is bounded to the Windows notification-icon limit. The balloon is
only shown after the first state, so startup is quiet.

## Experimental overlay

The older topmost strip remains behind `VIBDOCK_OVERLAY=1` for experiments and
other placements. It is **not** started by the launcher and is not the default.
The Windows 11 taskbar may paint it behind the shell; do not treat its
`PrintWindow` output as proof of on-screen visibility.

## Known limits

- The tray icon lives in the notification area, not the blank left/centre
  taskbar band.
- Windows can collapse new notification icons into the overflow flyout; pin the
  VibDock icon if needed.
- The icon shows the last completed action, not a live per-button state.
