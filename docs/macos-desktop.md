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

Settings can pin up to eight currently verified native applications. Saved bundle
locations and optional bundle IDs live in `apps-macos.json`, separate from Windows
`apps.json` and microphone configuration. The floating panel, menu-bar menu and
Dock menu show each saved label, native 32px icon and application-presence state.
Running requires a fresh verified match; Not running requires fresh complete
absence; partial or stale absence is Unknown. UI expiry clears icons as well as
state. This is read-only presence, with no software control or audio inference.
Audio, global shortcuts and voice actions remain unavailable on macOS.

The disposable helper enumerates NSWorkspace applications for the current user,
rechecks native application identity and kernel process creation time, and verifies
canonical bundle/executable locations before publication. Kernel start seconds
and microseconds bind transient selections; optional AppKit launchDate is an
additional check, so direct executable launches do not require LaunchServices
metadata to establish identity. It never launches a
saved application or loads arbitrary icon URLs. Icons are rasterized through
AppKit to fixed 32×32 RGBA PNGs, with no source metadata. The helper has a
three-second parent deadline, a four-second self-watchdog and an stdin parent-
death gate. Owned output is unlinked immediately and bounded to 512KiB.
See the [shared persistence/privacy contract](resident-software.md).

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
The actual root view paints the system window background. Acceptance captures
the same native view in light and dark appearances and requires opaque,
distinct backgrounds; evidence images are not post-composited.

Software acceptance distinguishes a deliberately offline saved fixture from the
actual running bundled VibDock application. The latter must be independently
observed with a native icon before lifecycle acceptance begins. It verifies
the expected original teal logo's observed pixels, rather than accepting any
decodable image as correct branding. It also verifies
software rows in the panel/menu-bar/Dock menus and expiry-to-Unknown/icon removal.
Browser checks exercise the real isolated Mac service at desktop and mobile
widths, pin a genuinely enumerated application, restart the service and verify
its saved row, remove both live/offline pins, and check one-use handoff and
reopening saved settings.
These tests do not prove arbitrary third-party playback or application work.

Real default-browser launch, physical pointer/focus behavior, multi-monitor/DPI,
light/dark desktop appearance, Finder launch and installed/updated app experience
still require logged-in desktop acceptance. No Accessibility, Automation, screen
recording, microphone permission, login item, certificate or Apple credential is
created or accepted by this implementation or its tests.

## Dependencies

AppKit bindings are macOS-only, with narrow framework features and exact direct
versions: objc2 0.6.3, objc2-foundation 0.3.2, objc2-app-kit 0.3.2. Cargo.lock also
pins objc2-core-foundation 0.3.2, objc2-encode 4.1.0, dispatch2 0.3.1 and bitflags
2.13.2, plus libc 0.2.190 for typed process identifiers. Their published manifests permit MIT; the optional Apache/Zlib alternatives
are not needed here. Notices are in [licenses](licenses/README.md). Apple SDKs
remain supplied by the existing Xcode runner environment, not redistributed.
