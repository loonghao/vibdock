# macOS native resident candidate

Current source builds an actual `VibDock.app` with two sibling Mach-O entries:
the native launcher and the existing loopback service. No Python or Node runtime
is required to use the packaged application. Candidate archives are not Developer
ID signed or notarized; they are not a claim of frictionless Gatekeeper acceptance.
Do not disable macOS security controls to run a candidate.

## Native behavior

- Open `VibDock.app` to show a movable floating panel, original-logo menu-bar item
  and VibDock's own Dock entry. No fixed screen corner is assumed.
- The menu bar, application menu and Dock menu offer Show panel, Hide panel,
  Settings and Quit. Closing the panel hides it and leaves the service resident.
- Reopening the app restores the existing window. A per-user advisory file lock
  also protects direct binary launches and multiple installed copies. A random,
  launch-local Show-only Unix socket message reaches the lock-owning panel; it
  cannot launch a program, open a URL, change configuration or control audio.
- Settings obtains a one-use browser code from the proved loopback service.
  Service requests run on one bounded worker, not the AppKit main thread. Unknown
  or expired liveness is shown as unknown; no bearer token appears in native UI.
- Quit asks the verified service to stop. Failure leaves controls available for
  retry. AppKit system termination waits for that bounded cleanup, and the native
  controls, worker and owned reopen socket are released before allowing exit.
- The original supplied icon PNGs are packed unchanged into ICNS. The menu-bar
  symbol uses the original monochrome shape as an AppKit template, adapting to
  system appearance without remote image requests.

macOS application pinning, third-party software icons and playback/voice state
are not implemented by this native shell. Windows output/session identities are
not reinterpreted as macOS applications. Audio, global shortcuts and voice actions
remain unavailable on macOS; process focus is never labeled working or recording.

## Evidence and limits

Exact-head CI builds on macOS, Windows and Linux, runs core regressions and checks
bundle contents, native architecture, executable bits, original-art hashes and
license notices. Offline package tests use explicitly labeled Mach-O header
fixtures; those files are never executed and are not native GUI evidence.

The native macOS acceptance runs the real bundled executable and AppKit event
loop with a fresh temporary state directory and private port. It exercises actual
native menu selectors, show/hide/close/reopen, a second launcher process, a real
service identity proof, Settings-code issuance and authenticated Quit. It captures
the application's own native view via `NSBitmapImageRep`; this is an AppKit view
render, not a full-desktop/menu-bar screenshot or physical pointer test. Reports
must name the source revision, binary hash, OS version, PID and native window.

Real default-browser launch, physical pointer/focus behavior, multi-monitor/DPI,
light/dark desktop appearance, Finder launch and installed/updated app experience
still require logged-in desktop acceptance. No Accessibility, Automation, screen
recording, microphone permission, login item, certificate or Apple credential is
created or accepted by this implementation or its tests.

## Dependencies

AppKit bindings are macOS-only, with narrow framework features and exact direct
versions: objc2 0.6.3, objc2-foundation 0.3.2, objc2-app-kit 0.3.2. Cargo.lock also
pins objc2-core-foundation 0.3.2, objc2-encode 4.1.0, dispatch2 0.3.1 and bitflags
2.13.2. Their published manifests permit MIT; the optional Apache/Zlib alternatives
are not needed here. Notices are in [licenses](licenses/README.md). Apple SDKs
remain supplied by the existing Xcode runner environment, not redistributed.
