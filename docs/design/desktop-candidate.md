# Desktop candidate and acceptance

## Windows source

The resident entry uses the existing `windows` 0.61 bindings, named mutex, shared
service core and pinned proof/control protocol. It adds no desktop framework.
A normal small top-level window provides a movable panel and taskbar entry.
The notification-area menu is the persistent way back after close-to-hide.
Native controls use system colors and font; the original supplied artwork is
embedded through the installed Microsoft Windows SDK resource compiler.

State vocabulary is deliberately narrow: a fresh signed service proof means
**connected**, not **working**. A missing/expired proof means **unavailable / unknown**.
No app voice or microphone state is inferred. The status worker never hands the
service token to window text or notification metadata. Settings/Exit use the
existing verified-instance functions; there is no reconnect after proof.

## Automated evidence

- `cargo fmt --all --check`, `cargo clippy --locked --all-targets -- -D warnings`,
  `cargo test --locked --all-targets` on Windows, Linux and macOS.
- `scripts/test-windows-desktop.ps1` launches actual Windows release executables
  with a fresh state directory and port. It checks HWND/PID, rendered panel,
  actual context-menu labels, close-to-hide, second-launch singleton handoff,
  Escape, unchanged service record and authenticated Exit. It uses Win32 messages
  and PrintWindow captures. This is native rendering and lifecycle evidence,
  not physical pointer, tray accessibility, default-browser or DPI acceptance.
- `scripts/test-web-ui.cjs` uses a real browser against a real isolated service,
  without mocked routes. It checks 1280/390/320 px layouts, the exact supplied
  inline SVG bytes, accessible logo text, absent external requests and JS errors.
  Separate light/dark captures render the original supplied variants, not a new logo.
- Evidence reports identify the source revision and built executable hash. Old
  reports/captures are removed before a rerun so a failed run cannot inherit PASS.
  State files and browser credentials are never uploaded.

## Remaining Windows acceptance

Before recommending unattended desktop use, verify on an actual interactive host:

1. Double-click; no console flash, one panel and one tray icon. Move across monitors
   at 100/150/200% DPI, including negative coordinates and taskbars on different edges.
2. Pointer and keyboard context menus, Settings in the chosen default browser,
   one-use fragment erased, session expiration/reopen and inaccessible browser failure.
3. Save a real microphone/shortcut pair, reopen Settings, restart the service,
   and confirm the same endpoint and binding remain. Missing endpoints must fail
   without substitution. Confirm actual hardware delivery/readback independently.
4. Light/dark/high-contrast tray icon, taskbar restart recovery, focus while typing,
   minimize/close/hide, repeated launches and authenticated Exit with error recovery.

## macOS next phase (planned, not implemented)

The current macOS artifact is CLI-only; neither menu-bar GUI nor audio is implied
by a successful macOS build. A native adapter can use AppKit `NSStatusItem`,
`NSMenu`, `NSPanel` and `NSApplication` through scoped `objc2`/`objc2-app-kit`
bindings. Keep all AppKit objects and callbacks on the main thread; initialize
after applicationDidFinishLaunching. A menu-bar resident accessory need not have
a permanent Dock entry; opening its control panel can use the appropriate regular
activation policy, while window close preserves the resident service. Quit must
call the same authenticated service shutdown contract.

Before introducing it, add a proper `.app` bundle (Info.plist, original icon,
launcher and sibling CLI), verify framework/dependency licenses and minimum OS,
and test reopen/single-instance/quit on a real macOS GUI session. Distribution
signing/notarization or new credentials require their own setup; do not weaken
Gatekeeper or claim a CLI tarball is a native desktop release.

Per-software entries require a separate persistent configuration schema and live
adapters. Bind each action to stable app identity plus the exact current process,
audio endpoint/session or connection instance. Detection is not connection;
connection is not working. Unknown and unavailable must remain visible, and
controls without verified support stay disabled. Do not invent app logos or
reuse another project's artwork.

References: [Microsoft notification area](https://learn.microsoft.com/en-us/windows/win32/shell/notification-area),
[Apple NSStatusItem](https://developer.apple.com/documentation/appkit/nsstatusitem),
[objc2 AppKit bindings](https://docs.rs/objc2-app-kit/0.3.2/objc2_app_kit/).
