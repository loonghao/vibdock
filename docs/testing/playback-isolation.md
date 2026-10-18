# Two-application playback isolation harness

`scripts/test-playback-isolation.ps1` is an opt-in, Windows-only harness that
reproduces the two-application playback isolation check against real Windows
Core Audio sessions. It is the repeatable form of the manual check described in
application-recognition acceptance work; it is not part of hosted CI.

## What it does

1. Writes two short, distinct 16-bit PCM WAV tones (440 Hz and 660 Hz) to a
   fresh temporary directory.
2. Starts two real `pwsh` player processes, each looping one tone with
   `System.Media.SoundPlayer.PlayLooping()`, and records both PIDs.
3. Runs `vibdock app-devices` and requires exactly one active render endpoint.
   If there are zero or several, it fails rather than guessing.
4. Runs `vibdock app-list --endpoint <id>` and selects exactly the two sessions
   owned by the player PIDs. A missing or duplicate owner match is an error,
   never a fallback to another session.
5. Mutes session A with `vibdock app-mute`, then asserts `vibdock app-status`
   reports A with `muted=true` and B with `muted=false` and `active=true`.
6. Unmutes A with `vibdock app-unmute` and asserts A reports `muted=false`.
   A final `app-list` confirms both exact sessions are still present, unmuted
   and active.
7. Always stops both player processes and deletes the temporary directory in
   `finally`, including on failure. If A was muted when a later assertion
   failed, it attempts an unmute before stopping the players.

## Safety properties

- **Session-scoped only.** An allowlist permits only `app-devices`, `app-list`,
  `app-status`, `app-mute` and `app-unmute`. Device-level verbs (`devices`,
  `status`, `mute`, `unmute`, `toggle`) and `--device` are rejected, so the
  endpoint master mute and microphone state cannot be changed by this harness.
- **Exact identities.** The endpoint must be the only active render endpoint,
  and each player PID must own exactly one session. Every readback rechecks the
  endpoint id, session instance id and PID before a state assertion.
- **No fallback.** There is no default endpoint, no "first session" choice and
  no silent retry against another target.
- **Cleanup.** Player processes are killed by process tree, temporary files are
  removed, and an accidental mute is reversed best-effort.

## Requirements

- Windows 10/11 x64 with an active render endpoint and working Windows audio.
- PowerShell 7.
- A built `vibdock.exe`; the default is `target\debug\vibdock.exe` relative to
  the repository root. Run `cargo build --locked` first.
- An interactive desktop session. **Do not add this to hosted CI**: hosted
  runners have no usable audio hardware, and the script refuses to run when it
  detects hosted GitHub Actions.

## Run

```powershell
cargo build --locked
pwsh -NoProfile -File scripts/test-playback-isolation.ps1
```

Use another binary or a longer wait for slow audio drivers:

```powershell
pwsh -NoProfile -File scripts/test-playback-isolation.ps1 `
    -Binary .\target\debug\vibdock.exe `
    -TimeoutSeconds 30
```

Exit code `0` means every assertion passed. Any failure prints a `FAIL:` line,
still runs cleanup, and exits nonzero.

## Evidence

On success the harness prints the exact render endpoint id, both player PIDs,
both session instance ids, and the raw CLI JSON for:

- the initial `app-list` enumeration;
- `app-mute` on A and the `app-status` readback for A and B;
- `app-unmute` on A and the final `app-status`/`app-list` readbacks;
- a final summary JSON with the endpoint, player/session mapping,
  `endpoint_master_mute: "untouched"`, and the exact list of commands issued.

## Scope limit

This is **state-level isolation**, not audible meeting isolation. It verifies
that muting one application's playback session leaves another application's
session unmuted and active on the same endpoint, and that the endpoint master
mute is never addressed. No meeting is joined, no voice session is started, and
audible output is not measured. It also does not test input/microphone isolation
or meeting applications' own audio processing.
