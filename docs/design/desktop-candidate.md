# Desktop candidate and acceptance

## Windows source

The resident entry uses the existing `windows` 0.61 bindings, named mutex, shared
service core and pinned proof/control protocol. It adds no desktop framework.
A hidden top-level window owns the notification icons and receives Explorer
restart broadcasts. Launch, relaunch and primary-icon clicks open authenticated
Web Settings directly. The old Windows panel is removed; closing the browser
leaves the tray and service running. The original supplied artwork is
embedded through the installed Microsoft Windows SDK resource compiler.

A separate compact software-logo strip uses named native buttons and playback
badges above the primary work-area bottom edge. It does not activate on clicks;
Settings and exact-session menus remain the controls. Missing/incomplete/expired
observations show unknown, and no badge claims voice activity. Layout, badge
classification and passive styles have unit coverage; actual pointer, tooltip,
focus, DPI and multi-monitor behavior still need host acceptance.

State vocabulary is deliberately narrow: a fresh signed service proof means
**connected**, not **working**. A missing/expired proof means **unavailable / unknown**.
No app voice or microphone state is inferred. The status worker never hands the
service token to window text or notification metadata. Settings/Exit use the
existing verified-instance functions; there is no reconnect after proof.

## Automated evidence

- `cargo fmt --all --check`, `cargo clippy --locked --all-targets -- -D warnings`,
  `cargo test --locked --all-targets` on Windows, Linux and macOS.
- `scripts/test-windows-desktop.ps1` launches actual Windows release executables
  with a fresh state directory and port. It checks HWND/PID, a hidden owner
  without old panel controls, Settings/status/Exit menu labels, software-icon
  exact-row menus, second-launch singleton handoff, unchanged service record
  and authenticated Exit. It also checks a labelled saved offline software
  fixture, its unknown state, a completed native observer result and ordinary
  helper cleanup. It uses Win32 messages
  and PrintWindow captures. This is native rendering and lifecycle evidence,
  not physical pointer, tray accessibility, default-browser or DPI acceptance.
  Earlier panel/close-to-hide captures describe the superseded Windows flow
  and cannot establish acceptance of the current tray-first flow.
- `node scripts/test-binding-ui.cjs` checks service-mode binding and software
  microphone-association DOM logic with fixtures. It does not enumerate real
  microphones, open a default browser or perform audio actions.
- `scripts/test-web-ui.cjs` uses a real browser against a real isolated service,
  without mocked routes. It checks 1280/390/320 px layouts, the exact supplied
  inline SVG bytes, accessible logo text, absent external requests and JS errors.
  It reads/removes a saved offline software fixture and verifies the committed
  file; this fixture never represents real audio. Separate light/dark captures
  render the original supplied variants, not a new logo.
- Evidence reports identify the source revision and built executable hash. Old
  reports/captures are removed before a rerun so a failed run cannot inherit PASS.
  State files and browser credentials are never uploaded.

## Remaining Windows acceptance

Before recommending unattended desktop use, verify on an actual interactive host:

1. Double-click; no console flash or old resident panel, one tray owner and an
   authenticated Settings page. Check tray/menu behavior at 100/150/200% DPI,
   including negative coordinates and taskbars on different edges.
2. Pointer and keyboard context menus, Settings in the chosen default browser,
   one-use fragment erased, session expiration/reopen and inaccessible browser failure.
3. Save a real microphone/shortcut pair, reopen Settings, restart the service,
   and confirm the same endpoint and binding remain. Missing endpoints must fail
   without substitution. Confirm actual hardware delivery/readback independently.
   Pin a software entry, associate its exact microphone, restart and read the
   association back. Verify its explicit microphone controls against that endpoint;
   a missing endpoint must stay unavailable. This does not verify application
   input routing or voice-session start/stop, which remain `not_implemented`.
4. Light/dark/high-contrast tray icon, taskbar restart recovery, focus while typing,
   browser close/reopen, repeated launches and authenticated Exit with error recovery.
5. Play real software audio, pin its observed output/session, verify its native
   icon and active/idle/mute observations, restart, and exercise process/session
   replacement and missing outputs. No stale observation may claim a current
   state; partial enumeration must not report all sessions muted or idle.
6. With two identifiable applications playing on the same output, record their
   playback, output-master and microphone mute states. Right-click one saved
   software icon, select its exact displayed session/PID, and explicitly choose
   Mute. Require both the matching native readback receipt and independently
   observed silence of only that session; the other application, output-master
   and microphone states must remain unchanged. Explicitly Unmute the same
   session and verify again. Multiple sessions must require an explicit choice;
   an expired, replaced or offline session must disable/reject the old action.
   A timed-out or unconfirmed receipt is not success and is not proof that no
   write occurred. Inspect the actual state before making another request.

## macOS native source and remaining acceptance

Current source packages an unsigned `VibDock.app` with the original supplied
icon, native launcher and sibling CLI. AppKit `NSStatusItem`, `NSMenu`, `NSPanel`
and `NSApplication` stay on the main thread. A persistent menu-bar entry and
Dock entry restore the same floating panel; closing hides it, while Quit uses
the authenticated service shutdown contract. Saved application-presence rows
and native icons appear in the panel, menu-bar menu and Dock menu.

Hosted macOS acceptance runs the actual bundled executable and isolated service,
checks native menu selectors, singleton reopen, close-to-hide and both Quit
paths, and captures the real own-content view in light/dark appearances. It
checks opaque system backgrounds and the observed original teal logo pixels.
Browser acceptance pins an actually enumerated application and verifies its
saved row across service restart and removal. These are native widget and
application-presence checks, not physical pointer, Finder/default-browser,
full-desktop or audio-control evidence.

Physical desktop focus/monitor behavior, localized native wording and visual
polish still need separate acceptance. macOS audio, voice and global shortcuts
remain unsupported. Developer ID signing/notarization is not configured; no
credential, permission or Gatekeeper bypass is part of this candidate.

See the [software contract](../resident-software.md),
[native macOS behavior and evidence limits](../macos-desktop.md) and
[native identity and packaging plan](../macos-resident-plan.md).
Future software action adapters must preserve the following boundary: Bind each action to stable app identity plus the exact current process,
audio endpoint/session or connection instance. Detection is not connection;
connection is not working. Unknown and unavailable must remain visible, and
controls without verified support stay disabled. Do not invent app logos or
reuse another project's artwork.

References: [Microsoft notification area](https://learn.microsoft.com/en-us/windows/win32/shell/notification-area),
[Apple NSStatusItem](https://developer.apple.com/documentation/appkit/nsstatusitem),
[objc2 AppKit bindings](https://docs.rs/objc2-app-kit/0.3.2/objc2_app_kit/).
