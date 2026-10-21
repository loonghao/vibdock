# Tap/hold voice implementation status

This source snapshot is work in progress. It is not a qualified hardware or
input-method release. The existing service and device configuration have not
been changed by this development work.

## Intended behavior

- An opt-in AU05 button runs its existing Single action on each short release.
  The microphone action retains an explicit endpoint ID; two rapid taps toggle twice.
- A Long slot accepts explicit `hold_keys` and `long_press_ms` (150–5000, default550),
  with empty `steps`. It holds the configured input-method chord until release
  or cancellation and never also runs Single. No input-method chord is prefilled.
- The mode is stored in the existing revisioned `gestures.json`. Ordinary saved
  single/double/long macros retain their old sentinels and timing.
- Opt-in held buttons use bare F9/F10/F11, with global keyboard scope rather than
  physical-device identity. The service owns capture and held output on a separate
  worker so blocking macros cannot defer key release.
- This keyboard dispatch remains separate from endpoint mute, application playback
  mute, local speech recognition, and an application's internal voice session.

## Remaining correctness work before deployment

1. `src/web.rs` currently cancels by configuring an empty held-binding set before
   gesture mutations. This drops capture/reservation before a blocking firmware
   write, and new bare keys can be written before replacement capture succeeds.
   Separate cancellation from suppression, reserve/prepare replacement capture
   before provisioning, and preserve fail-closed behavior on setup failure.
2. `src/gesture_voice.rs` only rejects simultaneous held chords that share a key.
   Disjoint chords can still combine modifiers. Serialize held output globally;
   coordinate ordinary `send_keys` actions with active holds as well.
3. The Settings completion path checks root `error` but the service reports
   `hold_error` and per-slot `error`. Test and consume the actual wire format so a
   runtime failure cannot display setup completion.
4. Add an activation-error accessor/current fatal status; the service's cached
   `voice_error` must reflect worker disconnects. Do not let cached successful
   snapshots hide a dead worker. Recovery must not rearm a still-held suppressed
   sentinel or bypass permanent cleanup failure.
5. Add a bounded active-hold safety deadline (30 seconds) and release lockout for
   lost release events. An absent USB/hook key-up can currently hold indefinitely.
6. Finish regression coverage for those issues, run final-source workspace checks
   on Windows/Linux/macOS, then qualify exact-head hosted CI. Physical AU05,
   input-method behavior and audio remain separate acceptance gates.

## Checks already performed

Windows workspace tests and strict workspace Clippy passed on an earlier draft.
The input worker subsequently passed 16 focused mock/state-machine tests and
Windows library Clippy. The Settings contract tests passed. These checks occurred
on successive uncommitted drafts, so they do not qualify this final source tree.
The first formatting check found changes; selected files were formatted later,
but a final all-workspace formatting check is still required. No native input,
firmware, microphone, input-method session or installed service was exercised.

## Separate phone-control follow-up

Keep phone networking and pairing in a separate change. A phone control panel
must target an explicitly selected local executor: a remote host's SendInput
cannot control the user's laptop. Preserve the existing authenticated loopback
service and its Host/Origin policy.

The smallest runnable proposal is an opt-in HTTPS private-LAN Pad on the laptop,
with a separate restricted phone pairing principal, persisted executor identity
and approved action-ID/digest allowlist, consume-before-dispatch idempotency, and
status readback. Do not install a VPN, open a public listener, modify firewall
rules, distribute the service token or silently select another executor.

Reuse `browser_auth.rs` expiry/replay patterns, `resident_actions.rs` request
receipts, the saved-action Pad executor, and `stt.rs::transcribe_samples`.
`Cargo.lock` already contains rustls transitively, but TLS is not implemented in
this snapshot. Existing TCP transport clones streams and cannot simply clone TLS
state. Bounded binary WAV upload needs its own strict framing/size validation.

The phone can hold its own audio recording and upload validated PCM WAV on
release; cancellation/hidden/pagehide/disconnect discard recording and stop tracks.
Do not add a remote keydown protocol to a finite-tap prototype. Default builds lack
STT: without the feature/model return unavailable, never synthetic text. Remote
recognition remains explicitly unconfigured until a trusted channel and target
routing are selected. Certificates/phone trust, laptop deployment and live network
acceptance require explicit setup; none has been performed by this snapshot.
