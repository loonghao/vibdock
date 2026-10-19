# AU05 Vibe Key direct bindings

Status: implemented in the current Windows source. Live acceptance on
2026-10-19 against a physical AU05 (firmware 4.4.0) covered the first-run
default layout, all three default buttons producing their host effects, and a
physical application-bound button press appearing in the recent-trigger log.
Earlier the same day a temporary protocol probe verified enumeration, slot
readback, TEA framing, and one write/readback. Studio coexistence, replacement
devices, power-cycle persistence, and unbind clearing remain unverified. The
dated record is the [AU05 acceptance record](../testing/au05-acceptance.md).

## Problem

Mapping the three round buttons of an Ulanzi AU05 Vibe Key ("Vibe Key") has
required Ulanzi Studio: the buttons are configured in the vendor application,
and the device then emits whatever keyboard chord that application stored in
its firmware. VibDock captures global trigger chords and dispatches to saved
applications, but a user who does not run Studio has no way to make a physical
button carry a trigger.

This feature removes the Studio dependency for those three buttons and covers
two different jobs:

1. **Application-binding mode.** A saved per-application shortcut names a
   device button. VibDock writes that binding's registered trigger chord into
   the button's firmware. The button emits the chord, VibDock's existing global
   registration captures it, and dispatch follows the saved `foreground` or
   `running` scope.
2. **Direct firmware-chord mode.** A chord or single key is written straight
   into the button's firmware, with no VibDock trigger and no dispatch. The
   button emits the keys itself; VibDock is not involved. This exists for
   chords Windows will not let VibDock register (notably `Ctrl + Win`) and for
   bare keys such as `Enter` or `Escape` that cannot be a registered trigger.

On first run with no saved `hardware.json`, the service writes a built-in
default layout using the direct mode: button 1 = `Ctrl + Win` (微信语音, WeChat
Input voice), button 2 = `Enter` (回车), button 3 = `Escape` (取消). A button that
already carries an application binding keeps that binding instead.

It is deliberately not more than that:

- It is not a voice integration. A `provisioned` button only proves that the
  firmware setting was written and read back; it says nothing about voice
  state, application state, or whether a chord was accepted.
- It does not install, launch, or control Ulanzi Studio.
- It does not flash firmware or update the device.
- It does not change audio routes. The AU05 microphone is a normal Windows
  capture endpoint.
- It manages only the three round buttons (device slots 0-2). The knob, the
  built-in microphone, lighting, standby timers, and every other device setting
  are out of scope and are never written.

## Device and transport

| Property | Value |
| --- | --- |
| USB vendor ID | `0xFFF1` |
| USB product ID | `0x00DD` |
| Vendor HID collection usage page | `0xFFFC` (top-level) |
| HID report ID | `0x55` |
| Report size | 64 bytes (1 report ID byte + 63 ciphertext bytes) |
| Product string | `AU05` (observed) |
| Observed interface token | `MI_03` on the tested unit |

The device exposes several HID collections (keyboard, consumer control, mouse,
and the vendor channel). The adapter enumerates present HID interfaces, opens
each one read-only to read its attributes and capabilities, and keeps only the
collection whose vendor/product pair matches **and** whose top-level usage page
is `0xFFFC`. Filtering on the vendor/product pair alone would select the first
keyboard collection and silently write frames to the wrong interface. The
interface number is not a filter: `MI_03` was observed on the tested unit, but
it can vary with the receiver port and driver, so the usage page is the
identity that matters.

The vendor channel also carries unsolicited broadcasts (battery, heartbeat), but
the shipped feature treats it as request/response: it ignores frames whose
header does not match the reply it is waiting for and times out when no reply
arrives. Silence is never treated as success.

## Frame encoding

Every frame is one 64-byte report:

```text
wire report:  [0x55][ciphertext byte 0] … [ciphertext byte 62]      = 64 bytes
plaintext:    [b0 b1 b2 b3][payload …][zero padding to 64 bytes]
```

1. Build a 64-byte plaintext: a 4-byte command header (`b0..b3`), then the
   payload, then zero padding.
2. TEA-encrypt every complete 8-byte block in place. The block words are
   little-endian `u32`; the cipher is TEA with 32 rounds, delta `0x9E3779B9`,
   and a fixed 16-byte key published by the reference implementation
   (`0xCAA5BACA`, `0xBC2A8A6D`, `0xCA5A9EBA`, `0x9BB88BCA` as four
   little-endian `u32` words).
3. Put report ID `0x55` first, then the first 63 bytes of the 64-byte
   ciphertext. The final ciphertext byte never reaches the wire, so the last
   seven wire bytes are not a complete decryptable block and are not used for
   any field VibDock reads.

Replies use the same encoding. The device sets the high bit of the first
plaintext byte, echoes `b1`/`b2` from the request, and uses `b3` for the reply
direction. VibDock matches a reply by the high bit plus `b1`/`b2`; `b3` is not
compared.

The wire format and the fixed key were cross-checked against the MIT-licensed
[OpenVibeKey](https://github.com/palaemonboy/OpenVibeKey) reference
implementation (frame assembly, TEA key/rounds, report ID, usage page) and then
confirmed by decrypting reports captured from the physical device. The probe
also reproduced the TEA encoding, wrote a shortcut to a button, and read the
stored slot back. That is protocol evidence; it is not by itself evidence that
the Rust adapter's own HID path works on hardware. The later live acceptance did
exercise the shipped service's write/readback path for the default layout.

## Command frames

Plaintext payloads below start at plaintext byte 4.

| Operation | Request header `b0 b1 b2 b3` | Request payload | Reply data used |
| --- | --- | --- | --- |
| Read shortcut slot | `01 06 50 01` | `[button]` | `[button, 0x01, count, (pageSign, value) × count]` |
| Write shortcut slot | `01 06 50 04` | `[button, 0x01, count, entries…]` | none parsed; success is established by readback |
| Read fixed function | `01 06 10 01` | `[0x00, button]` | `[0x00, button, function]`, `0` means none |
| Clear fixed function | `01 06 10 04` | `[0x00, button, 0x00]` | none parsed |
| Keep-alive heartbeat | `06 01 23 00` | none | none |
| Read battery (probe only) | `01 01 02 01` | none | voltage `u16` little-endian, percent `u8` per the reference layout |

Both provisioning modes use the same "write shortcut slot" frame. Direct mode
is not a second device protocol: it is a second way to derive the entry list
that goes into that frame. The shipped feature only writes and reads the
shortcut and fixed-function slots; battery and heartbeat frames were decrypted
by the probe, and the service does not report battery state.

Buttons are addressed by zero-based device slot (`0`-`2`), displayed as 按钮
1-3 and as "Button 1"-"Button 3" in the CLI status view. The CLI's
`--device-button` and `--button` take `1`-`3` and convert to the zero-based
slot; the API fields `device_button` and `button` are zero-based.

### Shortcut entry encoding

Each shortcut entry occupies two bytes. The wire layout is the same for writes
and readbacks: `[page | (sign << 7), value]`. Page 2 is the keyboard usage page;
page 3 is the modifier page. The high bit of the first byte marks the left side
of a modifier.

| Entry | Wire bytes `[a, b]` | Meaning |
| --- | --- | --- |
| Left Control | `83 01` | page 3, sign 1, modifier value `0x01` |
| Left Shift | `83 02` | page 3, sign 1, modifier value `0x02` |
| Left Alt | `83 04` | page 3, sign 1, modifier value `0x04` |
| Left Windows | `83 08` | page 3, sign 1, modifier value `0x08` |
| Keyboard key | `02 usage` | page 2, the key's HID usage |

Left-side modifiers were verified on the physical device: the entry bytes
`83 01 83 04 83 02 02 42` (left Ctrl, Alt, Shift, F9) were written and read back
identically. The first-run default also verifies `83 01 83 08` (left Ctrl + left
Windows), while `02 28` (Enter) and `02 29` (Escape) are bare-key entries. A
right-side modifier stored by other software reads back with sign 0 (for example
`03 01`); VibDock writes only left-side modifiers and compares the side bit, so
such a chord is a different stored binding.

## Keys the AU05 can emit

Registered triggers (application-binding mode) still use the modifier-plus-key
grammar and require at least one modifier and one non-modifier key. Direct
mode additionally accepts a single bare key, because the device emits it as a
normal key press and no host hotkey is registered.

| Keys | Device usage |
| --- | --- |
| Letters `A`-`Z` | `0x04`-`0x1D` |
| Digits `1`-`9`, `0` | `0x1E`-`0x26`, `0x27` |
| `Enter`, `Escape`, `Backspace`, `Tab`, `Space` | `0x28`, `0x29`, `0x2A`, `0x2B`, `0x2C` |
| `-`, `=`, `[`, `]`, `\`, `;`, `'`, `` ` ``, `,`, `.`, `/` | `0x2D`, `0x2E`, `0x2F`, `0x30`, `0x31`, `0x33`, `0x34`, `0x35`, `0x36`, `0x37`, `0x38` |
| `F1`-`F12` | `0x3A`-`0x45` |
| `Insert`, `Home`, `PageUp`, `Delete`, `End`, `PageDown` | `0x49`-`0x4E` |
| `Right`, `Left`, `Down`, `Up` | `0x4F`-`0x52` |
| Windows key modifier `Win` | modifier page `0x08`; direct and dispatch only, never a registered trigger |

Modifier values are `0x01` Control, `0x02` Shift, `0x04` Alt, `0x08` Win.
VibDock writes the left side of each modifier.

Mode rules:

- A registered trigger is rejected with `400` if it contains `Win` or uses
  `Win` as its key half. Windows reserves `MOD_WIN` for shell bindings, so
  `RegisterHotKey` never receives it.
- A dispatch chord may contain `Win`; it is injected with `SendInput`.
- A bare key is rejected as a registered trigger but accepted by direct mode.
  A bare key is emitted into whatever application has focus, so use it
  deliberately.
- A bare `Win` direct chord is refused: it would open the Start menu rather
  than a user action. `Ctrl + Win` is accepted.

Rejected in both modes with an error naming the key:

- `F13`-`F24`, because the device cannot represent them;
- characters outside the table above, including non-ASCII characters;
- `+` without Shift in a combined chord, because the device would emit `=`
  for the same usage.

The API reports an unrepresentable chord as `400`; a persisted file is
validated on load, so a hand-edited binding cannot claim a button can emit a
chord it cannot.

## Two provisioning modes

### Application-binding mode

A saved entry in `shortcuts.json` may carry `device_button` (`0`-`2`). Its
trigger is registered globally by the existing registry and also written into
the button by `hardware`. On save the service persists and arms the binding
first, then writes the firmware slot. A firmware failure is reported in the
hardware status and does not roll back the saved binding; a later
`POST /api/hardware/sync` retries. The firmware write uses the same readback
verification as direct mode.

The adapter receives only the button, the parsed trigger, the trigger text, and
the binding's display labels; it never receives the executable path or the
dispatch chord.

### Direct firmware-chord mode

`hardware.json` stores up to three entries, independent of `shortcuts.json`:
one per managed button, each with a canonical chord and an optional label.
`POST /api/hardware/direct` saves and writes one chord;
`POST /api/hardware/direct/remove` clears one. `hardware-bind` and
`hardware-unbind` are the CLI projections.

- Direct chords are firmware-emitted: no host hotkey is registered and no
  dispatch is performed, so there is also no app-target check.
- A direct chord is any AU05-expressible chord, plus a bare key other than
  `Win`. `Ctrl + Win` is the canonical example because it is a common input
  method voice shortcut that VibDock cannot register.
- Direct mode is the only mode that accepts a bare key; registered triggers
  still require a modifier.
- The chord is canonicalised before saving (`Ctrl+Win` becomes `Ctrl + Win`;
  `esc` becomes `Escape`).
- Direct edits use the current `revision`: a stale revision returns `409`, an
  unknown field `400`, and unavailable storage `503`.

Per-button exclusivity: a button carries either an application binding or a
direct chord, never both. The service refuses the second kind with `409`:

- adding an application binding to a button that has a direct chord:
  "That Vibe Key button already has a direct chord; clear it first."
- saving a direct chord on a button that has an application binding:
  "That Vibe Key button already has an application shortcut; remove it first."

If both appear anyway (for example two hand-edited files), the status view
reports `failed` with a conflict message and provisioning writes neither value
for that button; the device keeps its previous content until the conflict is
resolved.

When an application binding is removed but a direct chord exists on the same
button (possible only through inconsistent hand-edited state), clearing the
binding rewrites the direct chord instead of blanking the slot.

### Direct state fields

Each entry in `GET /api/hardware` `buttons` carries:

| Field | Meaning |
| --- | --- |
| `direct_chord` | Saved direct chord, or `null` |
| `direct_label` | Optional label, or `null` |
| `direct_state` | `pending`, `provisioned`, or `failed` for the direct entry; `null` when no direct chord is configured |
| `direct_error` | Failure reason for the direct entry, or `null` |
| `state` | Combined per-button state: the application state, the direct state, `unbound`, or `failed` when both kinds are configured |
| `error` | Combined failure reason |

## Default layout and first-run seeding

`hardware.json` is seeded only when the file does not exist. The service mode
(not the read-only design preview) writes this layout at startup:

| Button | Chord | Label | Behaviour |
| --- | --- | --- | --- |
| 按钮 1 | `Ctrl + Win` | 微信语音 | firmware emits left Ctrl + left Windows |
| 按钮 2 | `Enter` | 回车 | firmware emits Enter |
| 按钮 3 | `Escape` | 取消 | firmware emits Escape |

Seeding rules:

- A button that already carries an application binding keeps that binding; its
  default entry is dropped from the seeded set and not written to
  `hardware.json`.
- The defaults are committed to `hardware.json` before the device write. If the
  device is absent, the layout is still saved and the button reports `failed`;
  a later 同步到设备 / `--sync` writes it.
- A `hardware.json` that exists but is empty does not re-seed defaults. Corrupt,
  oversized, or unsupported files preserve their bytes, disable hardware edits,
  and do not seed.
- The exact default wiring was read back on the physical device:
  button 1 entry bytes `83 01 83 08`, button 2 `02 28`, button 3 `02 29`.

The built-in layout is a starting point, not a fixed contract: all three entries
can be rewritten or cleared through the direct form, `hardware-bind`, or
`hardware-unbind`.

## Verification by readback

A firmware write is never reported as success on its own. Both modes call the
same `provision_entries` sequence:

1. Send a heartbeat, then wait about 80 ms.
2. Write the fixed-function clear for the button, then wait about 120 ms. A
   stored fixed function can suppress the shortcut, so it is cleared first.
3. Write the shortcut payload, then wait about 180 ms.
4. Read the shortcut slot back, waiting up to 1000 ms for a matching reply.
   The reply must parse and its button, entry count, entry order, usage values,
   and modifier side bits must equal the requested entries exactly.
5. Read the fixed function back, waiting up to 1000 ms. It must parse and
   report `function == 0` for the same button.

Only when both readbacks match does the service record `provisioned`.
Otherwise it records `failed` with a reason: no readback reply, a different
stored shortcut, a fixed function still set on the button, a malformed reply,
or a HID write timeout (the write wait is about 1200 ms).

This is a readback of device firmware settings. It is not a verified
application state, and it does not prove that Windows or the target application
will act on the emitted chord. The service reports application dispatch as
`DISPATCHED`, never as voice or application state.

### State model

The per-button `state` and, when configured, the separate `direct_state` share
one vocabulary. `direct_state` is `null` when the button has no direct chord;
the combined `state` is `unbound` when the button has neither kind.

- `unbound` (combined `state` only): neither an application binding nor a
  direct chord names the button.
- `pending`: a binding names it and the device is connected, but this service
  instance has not completed a write and readback for it yet. The button may
  still emit a chord stored by an earlier session.
- `provisioned`: the latest write for the current binding read both slots back
  identically. It is a recorded result, not continuous monitoring; unplugging
  the device does not retract it until a later attempt fails.
- `failed`: the latest attempt failed, or the device is absent and no result is
  recorded; `error` carries the reason. The saved binding or chord is kept.

The page additionally labels an unsupported platform or disabled control as
`unavailable`; the service expresses that as top-level `available: false`
instead of a per-button state.

## Why firmware emission is used for Win

`Ctrl + Win` is a real dispatch target, typically an input method's voice
shortcut, but it cannot be a VibDock trigger: `RegisterHotKey` does not receive
`MOD_WIN`, so a trigger containing it is rejected before registration rather
than saved and silently never fired.

A dispatch chord may contain `Win`, and VibDock can inject `Ctrl + Win` with
`SendInput`. Live acceptance on 2026-10-19 produced a `DISPATCHED` receipt for
that injected chord, but WeChat Input did not invoke its voice action. The same
chord emitted by the device firmware did invoke it. That negative result is why
the direct firmware path exists: firmware emission is a physical-key path,
while injected input was not accepted by this input method's hook. `DISPATCHED`
therefore means only that Windows accepted VibDock's input events; it never
means the target application acted.

## Recent-trigger contract

The service keeps a bounded, in-memory log so a physical press is observable
even when the target application ignores the chord. It is returned as
`recent_actions` by `GET /api/hardware` and `GET /api/config` (and by the
hardware sync/direct responses), newest last, at most 20 entries:

```json
{"sequence":7,"at":1790000000000,"source":"vibe_key","label":"Codex",
 "trigger":"Ctrl + Alt + Shift + F10","dispatch":"Ctrl + Shift + Space",
 "device_button":1,"state":"DISPATCHED","error":null}
```

- `sequence` shares the `last_action` counter and advances per completed
  trigger.
- `at` is milliseconds since the Unix epoch.
- `source` is `vibe_key` (an application binding that names a button),
  `application_shortcut` (an application binding without a button), or
  `microphone` (the global microphone toggle binding).
- `label` is the saved binding label for application sources and the configured
  microphone endpoint id for `microphone`; `trigger` is the registered trigger;
  `dispatch` is the injected chord for an application binding and `null` for a
  microphone action. `device_button` is zero-based and non-null only for
  `vibe_key`.
- `state` is the completed receipt (`DISPATCHED` for an application dispatch,
  `LIVE`/`MUTED` for a microphone readback), or `null` with `error` set.

The page shows the newest six entries in 最近触发 and mirrors the newest one in
the status line. The log exists only for the life of the service instance; a
restart clears it. It records what VibDock received and sent, never what the
application did.

Direct firmware chords do not appear in this log. The device emits them without
a host hotkey, so VibDock never sees the press; their effect is observable only
in the target application. Application-binding buttons do appear because their
trigger is registered globally and captured.

## Adapter boundary

The feature keeps the chord contract separate from the platform device I/O:

- `hotkey` owns the shared trigger grammar and canonical chord text. `Win` is
  parsed for dispatch chords but marked non-registrable.
- `au05` is a pure, platform-independent module: vendor constants, key mapping
  (including the Win modifier), direct-chord entry derivation, frame
  encode/decode, TEA, reply matching, and readback parsing.
- `hardware` is the platform adapter. It owns `DirectStore` (`hardware.json`),
  first-run seeding, both write paths, detection, overlapped HID I/O, readback
  verification, and the JSON status view. On other platforms it reports
  hardware control as unavailable.
- `app_shortcuts` decides which saved entries carry a button, validates
  registered triggers (rejecting `Win`), and hands the adapter a
  `HardwareBinding` with only binding id, display labels, button, trigger text,
  and parsed chord.
- `web` routes authenticated hardware reads and writes, joins `hardware` into
  shortcut responses, and owns the in-memory recent-action log. `shortcut_cli`
  projects the same API into `hardware-status`/`--json`/`--sync` and
  `hardware-bind`/`hardware-unbind`.

## Persistence and identity

Two files in the per-user VibDock state directory
(`%LOCALAPPDATA%\VibDock\` on Windows) hold the configuration:

- `shortcuts.json` (schema 1, revision, at most eight entries) holds application
  bindings: random id, label, executable location, registered trigger, dispatch
  chord, scope, and optional `device_button` (`0`-`2`).
- `hardware.json` (schema 1, revision, at most three entries) holds direct
  chords: one `{button, chord, label}` per managed button.

Constraints enforced on save and on load:

- at most one saved binding/direct chord per device button;
- a button cannot hold both kinds (see the conflict rules above);
- application triggers are unique across bindings and do not collide with the
  microphone trigger;
- a dispatch chord must not equal any VibDock trigger, so an injected chord
  cannot feed back into the global hotkey table;
- an application binding that names a button must carry an AU05-expressible
  trigger; direct chords must be AU05-expressible chords or bare keys.

Direct chords are validated against what the device can emit, and they are also
checked against VibDock's registered triggers (application triggers and the
microphone trigger): a direct chord equal to one of them is rejected with 409,
because the device-emitted keys would be captured by the global hotkey table as
well as reach the foreground application. The reverse check applies when a new
application trigger equals an existing direct chord. Direct chords still are not
checked against dispatch chords: those are only injected, never registered, so a
collision has no second host action.

Device identity is the vendor/product pair plus the `0xFFFC` usage page. No
serial number is stored or bound. Any attached AU05 with that vendor collection
is treated as the device, and only the first matching collection is used.
Application state is joined by binding id and direct state by button, so a
changed binding does not inherit an older result.

The service tracks, in memory, which buttons it has written during this service
instance. `POST /api/hardware/sync` rewrites every saved application binding and
every direct chord, and clears buttons that this instance previously managed and
that no longer carry either kind. It does not scan or clear firmware slots
written by Ulanzi Studio, another tool, or an earlier VibDock service instance.

## Failure and recovery

| Situation | What happens | What the user can do |
| --- | --- | --- |
| Device absent or receiver unplugged | Detection reports `connected: false`; unprovisioned buttons show `failed` with the not-found message, a previously `provisioned` result is kept until the next attempt | Plug in the receiver, power the device on, press any AU05 key, then refresh or `--sync` |
| Vendor channel asleep after the device's standby timeout | A HID write times out; the button shows `failed` and the message asks for a key press before retrying | Press any AU05 key, then retry sync |
| Ulanzi Studio holds or talks on the same collection | Writes or readbacks fail or time out; VibDock does not detect the Studio process | Quit Ulanzi Studio while VibDock owns the device |
| Shortcut readback differs from the requested entries | `failed` with "stored a different shortcut"; the saved binding/chord is kept | Retry sync; inspect with Studio only after quitting VibDock |
| Fixed function still set after the clear | `failed` with "kept fixed function N"; the shortcut may be suppressed | Retry sync; if it persists the slot was not cleared |
| Write accepted, readback missed | `failed`, never `provisioned`; the firmware may have stored the chord | Retry sync and confirm `provisioned` |
| Firmware write fails during save | The binding or direct chord is persisted first; the failure is reported in `hardware` and is **not** rolled back | Keep the visible configuration, or retry with 同步到设备 / `--sync` |
| First run with the device absent | The default layout is seeded into `hardware.json`; writes fail and buttons show `failed` | Connect the device, press a key, then 同步到设备 / `--sync` |
| `hardware.json` invalid or unreadable | Original bytes are preserved; hardware edits return `503` and no defaults are seeded | Fix or remove the file while the service is stopped, then restart |
| Application binding removed | The service clears or rewrites the button's firmware slot; direct chords are preserved | If the device was absent, the saved binding is still gone |
| Direct chord removed | The saved chord is deleted and remaining bindings are provisioned; the firmware slot is cleared only for a button this service instance previously wrote | Confirm by pressing the button; if it still emits the old chord, write a new chord or sync with the device present |
| Both kinds configured on one button | `POST` returns `409`; the view reports `failed` with a conflict message and provisioning skips that button | Remove the application binding or the direct chord, then sync |

A missing device does not roll back a removal. A written button keeps emitting
its chord while VibDock is not running; because it is ordinary keyboard input,
it then reaches whatever application is foreground. Choose a chord that is
harmless in other applications.

## Privacy and security

- The only mutation this path performs is writing a keyboard chord and clearing
  the fixed function for device slots 0-2. There is no shell, command, process,
  or file execution endpoint, and no arbitrary payload is sent to the device.
- Application-binding mode gives the adapter only the trigger chord, button
  index, and display labels; executable paths and dispatch chords stay in the
  service. Direct mode stores only button, chord, and optional label.
- Direct chords bypass the host entirely once written: the device emits them
  without asking VibDock or checking a target application. That is a behaviour
  choice, not an authority grant, and it cannot execute a command.
- `/api/hardware` and its sync/direct routes require the authenticated
  service's bearer token and the same `Host`/`Origin` checks as the other
  `/api` routes. The page shows the hardware card only in authorized service
  mode; the read-only design preview shows no hardware controls and cannot write
  to the device.
- `recent_actions` is memory-only, bounded, and returned to authenticated
  clients. It contains binding labels, trigger/dispatch chords, and the device
  button for the most recent completed triggers. It is not written to disk and
  is cleared on service restart.
- The fixed TEA key is public protocol material, not a credential: it
  obfuscates frames and does not make the device channel confidential or
  authenticated. Any local process that can open the same HID collection can
  read or change the stored chords. VibDock does not lock the collection.
- `shortcuts.json` contains executable paths, which are personal data. Do not
  attach it (or `hardware.json`) to public bug reports.

## Limits and open acceptance

Verified on 2026-10-19 (firmware 4.4.0):

- the first-run default layout was written and read back as `83 01 83 08`,
  `02 28`, `02 29`;
- the user confirmed button 1 invokes WeChat Input voice, button 2 sends Enter,
  and button 3 sends Escape;
- a physical application-bound button press appeared in `recent_actions` with
  its button, label, trigger, dispatched chord, and `DISPATCHED` receipt;
- the shipped service's write/readback path wrote those default entries and read
  them back; the same code path also clears and verifies the fixed-function
  slot before it reports `provisioned`;
- injected `Ctrl + Win` reported `DISPATCHED` but did not invoke WeChat Input,
  while the firmware-emitted chord did.

Not verified:

- coexisting with a running Ulanzi Studio;
- recognizing a replacement device (no serial identity is stored);
- a written chord surviving a full power cycle or receiver re-pair;
- sleep/wake recovery after the device's standby timeout on the shipped path;
- clearing a firmware slot on `hardware-unbind`, especially after a service
  restart or with the device absent;
- application-binding capture and dispatch exercised on each of the three
  buttons as a matrix (the log evidence covers physical presses, not a
  per-button matrix);
- hosted CI with hardware: CI has no AU05, so only protocol, validation, and
  persistence paths are covered there.

Also out of scope: the knob, the built-in microphone, lighting, standby timers,
firmware updates, audio routing, macOS, and Linux. The device setting is
firmware-owned; VibDock never flashes firmware. The microphone appears as a
normal Windows capture endpoint whose friendly name contains `AU05`; the
hardware view only uses that name for labeling and never selects or changes an
audio route.

The manual checklist and remaining evidence are in the
[AU05 acceptance record](../testing/au05-acceptance.md).

## References

- [OpenVibeKey](https://github.com/palaemonboy/OpenVibeKey), MIT-licensed
  independent Vibe Key implementation used to cross-check the wire format.
- [Vibe Key direct bindings in the README](../../README.md#vibe-key-au05-direct-bindings-windows-source)
- [AU05 acceptance record](../testing/au05-acceptance.md)
- [Resident software contract](../resident-software.md)
