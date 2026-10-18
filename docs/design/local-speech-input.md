# Local speech input

Status: M1 (`transcribe`), bounded M2 capture including hold-to-talk, and M3
focused-window insertion are implemented behind the optional `stt` Cargo feature
as `vibdock transcribe` and `vibdock dictate`. The default preview binary does not
include them. Silero VAD, configured-device selection, a target picker, clipboard
fallback and verified content/voice state remain open. The dev-only Python
prototype in `scripts/stt/` remains for model discovery/install checks.

## Goal

Offer offline dictation on Windows without a cloud transcription service: use a
local SenseVoice ONNX model, keep audio and text on the machine, and insert a
transcript only into a window the user explicitly targeted. Every step that can
change another application's content is a separate, user-visible action, and no
step may auto-send or silently install a model.

## Evidence

The DeepSeek Harness speech-to-text cache on the development host already holds
an INT8 SenseVoice ONNX model plus `tokens.txt`, and a Silero VAD ONNX model. A
prototype transcribed a Windows SAPI zh-CN WAV locally on 2026-10-18:

- source audio: 5.832 s, 22.05 kHz mono 16-bit PCM, resampled to 16 kHz in memory;
- transcript: `项目速记整理本周的工作要点，记录接下来的安排。`;
- SenseVoice tags: language `zh`, emotion `NEUTRAL`, event `Speech`;
- decode time: 0.128 s on CPU with two inference threads (first model load is
  much slower, roughly 1.6 s in the same prototype run).

This proves the cached model is reusable and CPU decoding is fast for short
utterances. The native Rust implementation (`sherpa-onnx` 1.13.8, optional `stt`
feature) produced the same transcript in 0.2007 s on the same WAV, including
automatic 22.05 kHz → 16 kHz resampling.

On 2026-10-18 the same build also:

- captured 2.013 s from the default microphone through CPAL and transcribed it
  locally (the ambient clip produced `I.`, not a meaningful accuracy check);
- registered `Ctrl + Alt + F12` for `--hold`, captured 1.407 s between key down
  and release, and transcribed on release; Escape during a second hold exited
  non-zero with `Cancelled; the captured audio was discarded.`;
- transcribed the 5.832 s WAV and inserted the exact Chinese sentence into a
  focused Notepad document with Unicode `SendInput`; UI Automation read back
  `项目速记整理本周的工作要点，记录接下来的安排。`;
- rejected a missing model, conflicting `--wav`/`--seconds`, and an out-of-range
  duration before recording.

It does **not** prove VAD segmentation, configured-device selection, continuous
listening, insertion into arbitrary applications, clipboard behaviour, or
resident memory cost. One synthetic zh-CN sample is not accuracy evidence for
other languages, noise conditions, or speakers.

## Model discovery

The native command and the transitional script use the same order, with
`--model-dir` and `VIBDOCK_STT_MODEL_DIR` as explicit overrides:

1. an explicit `--model-dir` or `VIBDOCK_STT_MODEL_DIR`;
2. the existing DSH SenseVoice cache, read-only:
   `%USERPROFILE%\.dsh\speech-to-text\sensevoice\models\sensevoice-onnx`
   (`$DSH_HOME` replaces `%USERPROFILE%\.dsh` when set);
3. the VibDock-owned directory: `%LOCALAPPDATA%\VibDock\models\sensevoice`.

A directory counts as a model only when it contains both `tokens.txt` and
either `model.int8.onnx` or `model.onnx`, with non-zero sizes. VibDock must not
write to, migrate, or delete the DSH cache: it is another product's data and may
be shared with an installed DSH voice-input feature. A VibDock-owned copy is
created only by an explicit install.

An explicit `--model-dir` overrides discovery and fails closed when it is not a
usable model. A typo must never fall back to a different local model or to a
network service. If discovery finds nothing, the helper reports the searched
paths and the explicit install route, and exits non-zero.

Unknown-provenance rules: a model found in either documented location is
accepted by location, not by a recorded checksum today. A VibDock-owned install
should pin per-file SHA-256 values and record provenance beside the model. A
future change should let the helper optionally require a pinned hash for a
discovered cache, but it must not reject a user's existing DSH cache by default.

## Explicit install

There is no silent fetch, no background prefetch, and no automatic retry. Model
installation is a separate action that shows its terms and requires consent:

- source: the pinned `k2-fsa/sherpa-onnx` release asset
  `sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17.tar.bz2`;
- transport: HTTPS only; a redirect away from HTTPS aborts the download;
- size: 163,002,883 bytes, enforced while streaming;
- SHA-256: `7d1efa2138a65b0b488df37f8b89e3d91a60676e416f515b952358d83dfd347e`,
  verified before extraction; any mismatch discards the archive;
- license: the upstream SenseVoice repository declares the MIT license. The
  converted model card publishes no separate license metadata, so the product
  must show the upstream license link and require the user to acknowledge it
  rather than asserting that the weights carry the same terms;
- consent: an explicit command flag or a UI confirmation that names the source,
  size, checksum, and license for the download being performed. Consent for one
  install is not consent for later automatic updates.

Downloads use a bounded temporary directory, verify the archive before
extraction, reject absolute paths and links in the archive, install only the
model and token files, and delete the archive afterwards. A failed or partial
install leaves no directory that later discovery would treat as a model.

The prototype implements this pinned download behind `--download` plus
`--consent-download`. It is not wired into the product; the runtime must not
expose a generic "download this URL" action.

## Native helper

The shape is a VibDock-owned command using the `sherpa-onnx` Rust binding, not a
shell, not a user-supplied executable path, and not a plugin system:

- fixed command vocabulary owned by VibDock (`transcribe --wav ...`), with no
  arbitrary argv pass-through;
- model directory selected only by explicit `--model-dir`, `VIBDOCK_STT_MODEL_DIR`,
  or local discovery; no network access for transcription;
- one bounded WAV in, one JSON object out, errors on stderr;
- no `cmd /c`, no Python runtime requirement, no PATH lookup at runtime;
- the optional dependency is feature-gated so the default preview binary remains
  small; a release that ships STT must build with `--features stt`.

The Python prototype is not required at runtime. It remains a developer path for
the consent-gated model install and for cross-checking the native result.

## Milestones

### M1 · Transcribe one WAV (native, behind `stt`)

`cargo build --features stt`, then:

```powershell
.\vibdock.exe transcribe --wav sample.wav --json
```

The command discovers an existing local model, emits one JSON object
(`text`, `seconds`, `model`), and exits non-zero with an actionable message when
the model or WAV is missing. No microphone, injection, hardware binding or
network access. Acceptance on the host recorded above transcribed the same
synthetic zh-CN sentence in 0.2007 s; missing-model and malformed-argument
paths are covered by unit tests.

The default `vibdock.exe` build does not include this dependency or command; it
reports that the build lacks local STT instead of downloading anything.

### M2 · Bounded capture and hold-to-talk (implemented)

`vibdock dictate --seconds N` opens the default input device through CPAL, records
for 1–30 seconds, downmixes to mono, resamples to 16 kHz in memory, and
transcribes. `vibdock dictate --hold --trigger <chord>` registers a VibDock-owned
global chord and captures until that key is released; Escape cancels and discards
the clip. Empty or silent captures fail closed; the command never falls back to
another microphone and does not write audio to disk. A 2.013 s live capture, a
1.407 s hold-trigger capture and a 5.832 s WAV all transcribe on the host
recorded below.

Still open: Silero VAD segmentation, use of the configured microphone identity
rather than the OS default, a long-running listener instead of one-shot hold, and
resident memory/latency measurements.

### M3 · Insertion into the explicitly focused window (implemented)

`vibdock dictate --wav ... --insert` (or captured audio with `--insert`) sends the
transcript as Unicode `SendInput` events to the foreground window. It never
presses Enter, clicks Send, or submits a form, and it checks the foreground
window before and after insertion. A detected focus change is an error; success
means input was sent, not that the target accepted, displayed, or submitted it.
There is no clipboard fallback and no per-application target picker yet.

A real Notepad check inserted the exact Chinese transcript and read it back
through UI Automation. This is input-routing evidence, not proof of application
semantics. The CLI is launched from a terminal, so `--insert` can target that
terminal if focus is not moved to the intended window; a tray/hotkey flow with a
target picker is still needed to make this safe for routine use.

## Privacy

- Audio, intermediate features, and transcripts stay on the local machine. No
  upload, no cloud fallback, no telemetry containing audio or text.
- Temporary audio is written only when a format needs a file, inside the user's
  temp directory, with a duration cap and a lifetime that ends with the
  transcription attempt. Leftovers from a crash are cleaned by age.
- Transcripts are not logged by default: not in service logs, tray text, crash
  reports, or diagnostics. Errors report the failing stage, not the recognised
  text. The dev prototype prints the transcript to stdout because that is its
  entire purpose; the runtime must not add transcript logging without an
  explicit, off-by-default diagnostic opt-in.
- No transcript history is persisted. A future history feature would be a
  separate, opt-in design with clear storage and deletion semantics.
- The model directory is shared data: read the DSH cache without modifying it,
  and never write VibDock state into it.

## Explicit non-claims

- Transcription output is **not** verified voice-session state. It does not say
  whether an application's voice mode is active, whether a shortcut started or
  stopped anything, or whether inserted text reached the target.
- There is no default-runtime STT support in this slice: `vibdock.exe` includes
  transcription, bounded capture and insertion only when built with
  `--features stt` and invoked explicitly with `transcribe` or `dictate`.
- VAD segmentation, a global hold-to-talk trigger, tray/Web dictation controls,
  a target picker and clipboard fallback are not implemented.
- Insertion is explicit (`--insert`) and verified only as "input was sent"; it is
  not verified content, submission, or application state.
- The prototype's `--download` is an explicit developer action, not automatic
  installation. The product runtime still has no download path.
- `lang`, `emotion`, and `event` are model output tags, not user intent or
  application state.
- No claim about accuracy across languages, accents, noise, or meeting audio;
  the measured run is one synthetic zh-CN utterance.
- The app-shortcut and STT paths are independent; local dictation does not
  dispatch a shortcut or claim a voice-session state.

## Open questions

- How the feature-gated native build is packaged per platform and per CPU
  baseline; the current preview build is not a release artifact.
- Whether discovery should require a recorded hash when the DSH cache changes,
  without breaking the shared-cache use case.
- Per-target insertion acceptance: Unicode `SendInput` works in Notepad, but
  elevated, sandboxed or custom editors may reject it; no target picker exists.
- Memory and latency of a resident model versus loading per utterance; the
  current command loads per invocation and has not been measured as a service.
- License review for redistributing weights versus download-on-demand, and the
  update/consent flow for a newer pinned model.

