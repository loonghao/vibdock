# macOS resident adapter plan (not implemented)

## First native slice

Use AppKit on its main event loop, through narrowly enabled `objc2` /
`objc2-app-kit` bindings. Reuse the existing proved loopback service and one-use
Settings handoff. Keep one `NSStatusItem` with the original supplied VibDock logo,
a menu with pinned software icons/status, Show panel, Settings and Quit. Retain and
remove the status item on the main thread; closing the panel must leave the service
and menu item alive. The Dock entry belongs to VibDock, with its own menu/summary;
we do not alter other applications' Dock icons.

List current user applications through `NSWorkspace.runningApplications` and read
`NSRunningApplication` metadata/icon. These APIs do not enumerate every process.
Bound entries and rasterize native icons to a small fixed image; never execute or
load an arbitrary user-provided bundle. Revalidate the live application object,
termination state and available launch identity before publishing a selection.
PID alone is insufficient, and launchDate may be absent. Do not persist live
process identifiers or use a bundle identifier alone as trusted executable proof.

A separately versioned macOS identity type should store a server-selected bundle
location plus optional bundle identifier, keeping Windows output IDs distinct.
Use the same random, short-lived selection, revision and atomic persistence
contracts. Expose independent fields for process presence, observed playback,
voice integration and action capability. `isActive` means frontmost, not working,
playing audio, microphone use or agent progress. Process presence must not be
presented as playback connectivity. Playback/voice remains unknown/unavailable
until a supported, explicitly reviewed backend can provide that evidence.

## Packaging and acceptance gates

1. Add an isolated macOS module and an actual .app bundle/Info.plist with supplied
   artwork and notices. Pin compatible crate versions in Cargo.lock after checking
   the selected bindings' licenses. No macOS dependencies are added by this plan.
2. Hosted macOS checks cover build, contracts, bundle contents and deterministic
   fixture states. They do not prove a logged-in menu-bar or Dock experience.
3. A logged-in macOS GUI test must observe double-click launch, one resident
   instance, menu icons, panel reopen, Settings handoff, persistent selection,
   stale/missing-app behavior, light/dark appearance and explicit Quit cleanup.
4. Signing/notarization and any required access to Apple credentials are separate
   release gates. No certificates, persistent credentials, login items,
   Accessibility/Automation permissions or audio-capture permissions are created
   or accepted automatically. Document unsigned candidate restrictions truthfully.
5. Playback/voice control is a later capability-specific change with actual
   readback. Do not use microphone taps or simulated shortcuts to manufacture a
   verified per-application state.

## Primary references

- [Apple NSRunningApplication](https://developer.apple.com/documentation/appkit/nsrunningapplication)
- [Apple NSStatusItem](https://developer.apple.com/documentation/appkit/nsstatusitem)
- [Apple NSDockTile](https://developer.apple.com/documentation/appkit/nsdocktile)
- [objc2 project and licensing](https://github.com/madsmtm/objc2)
- [Published NSRunningApplication bindings and inherited API contracts](https://docs.rs/objc2-app-kit/0.3.2/objc2_app_kit/struct.NSRunningApplication.html)
