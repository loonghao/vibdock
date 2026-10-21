# Configurable tap/hold voice candidate

This opt-in mode reuses `gestures.json`. A short release runs the button's Single
slot once; two rapid taps run it twice. A held Long slot contains explicit
`hold_keys`, empty `steps`, and `long_press_ms` (150–5000, default 550). It emits
one key-down sequence after the threshold and releases on up, Escape, physical
modifier intervention, reconfiguration, shutdown, or a 30-second safety deadline.
Long press and cancellation never run the Single slot. Ordinary gesture timing
and modified sentinels remain unchanged.

The held mode uses bare F9/F10/F11, globally reserved by the service and swallowed
by its keyboard hook. It cannot identify which keyboard emitted them. Output
never contains the ordinary Ctrl/Alt/Shift sentinel modifiers. The IME shortcut
must come from the user's confirmed configuration; none is prefilled.

A separate input worker owns held keys so a blocking macro or firmware write
cannot postpone release. Capture/reservations are prepared before firmware writes
and retained through cancellation and failed setup. Replacement capture reuses
existing reservations. Already-held triggers stay blocked until their real up.
Held output is serialized, including disjoint chords; ordinary service key
injection uses the same nonblocking lock and fails explicitly during a hold.
Failed key-up ownership is retained for cleanup retries, and a dead worker is
reported as disarmed until service restart. Settings checks root and per-slot
runtime errors before reporting setup completion.

## Windows preview setup and rollback

Extract the candidate ZIP into a new directory. Verify it with
`Get-FileHash .\vibdock-candidate-*.zip -Algorithm SHA256` against its `.sha256`.
Do not replace an installed copy. In a new PowerShell window:

```powershell
$env:LOCALAPPDATA = Join-Path $PWD 'preview-data'
$env:VIBDOCK_PORT = '38941'
.\vibdock-launcher.exe
```

This isolates files and service discovery; it does not virtualize Windows global
hotkeys or AU05 firmware. Do not run two hardware-owning services together. Close
an existing instance yourself before hardware acceptance. Never test microphone
toggle during a call. Open Settings, select the exact microphone, then use the
onboarding matrix: Single = toggle selected microphone; Long = hold shortcut.
Enter the IME's existing configured chord. Review the button/firmware confirmation
before saving. Do not add a Double slot to that button.

Acceptance: tap twice with state readback after each; hold and release; hold and
Escape; cancel while held; unplug/reconnect; test another ordinary gesture.
Check the recent events for HELD/RELEASED/CANCELLED and explicit failures.
These receipts prove service dispatch, not IME recording state. Hardware, WeChat,
Windows audio, hooks and laptop acceptance remain pending until observed.

Rollback: Exit VibDock through this preview's tray or use `vibdock service-stop`
from the same PowerShell environment. Release physical keys. Restore previous
firmware bindings explicitly from your saved configuration if you changed them.
Delete `preview-data` only if you no longer need its isolated settings; start your
previous installation normally. No existing user settings are overwritten.

## Validation scope

The candidate includes platform-independent tests for short taps, rapid taps,
threshold boundaries, repeat downs, cancellation, partial injection cleanup,
physical modifier ownership, disjoint concurrent holds, and lost key-up timeout
with release lockout. DOM fixtures exercise the actual runtime-error wire format.
Required workspace fmt, strict Clippy and tests run in hosted three-platform CI.
Windows resources require the Microsoft SDK; Linux GNU cross-check cannot package
the Windows launcher. Use the exact-head Windows CI candidate archive/checksum.

The phone Pad is a separate PR and does not install a virtual audio driver.
