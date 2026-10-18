# Local speech input

Status: milestone 1 is implemented behind the optional `stt` Cargo feature as
`vibdock transcribe`. The default preview binary does not include it; microphone
capture, hold-to-talk and text insertion remain M2/M3. The dev-only Python
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
automatic 22.05 kHz → 16 kHz resampling. It does **not** prove microphone capture,
VAD segmentation, hold-to-talk timing, text insertion, focus behaviour,
packaging, or resident memory cost. One synthetic zh-CN sample is not accuracy
evidence for other languages, noise conditions, or speakers.

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

### M2 · Hold-to-talk from the selected microphone

The user holds a VibDock-owned trigger, speaks, and releases; Silero VAD bounds
the utterance and a maximum duration caps it. The selected microphone identity
is used exactly as configured, with no silent fallback to another endpoint.
Recording is cancellable; failed or empty captures report a distinct state;
temporary audio is deleted after transcription; no transcript is inserted and no
application is changed while recording. Auto-send does not exist.

### M3 · Insertion into the explicitly focused window

Insert only into the window that is focused when the user triggers insertion,
show which target will receive text, and distinguish three states: transcribed,
dispatched, and verified. Insertion is its own user action, never an implicit
continuation of transcription. Never press Enter, never click a send button, and
never submit a form. A clipboard-based fallback must be labelled as clipboard
mutation and restore the previous clipboard or ask first. Focus can change
between capture and insertion; the helper must re-check the target and abort
rather than insert elsewhere.

Each milestone needs host acceptance with a real microphone and target
application. Unit tests and prototype events are not device evidence.

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
- There is no default-runtime STT support in this slice: `vibdock.exe` only
  includes transcription when built with `--features stt`, and it never captures
  a microphone or inserts text.
- No microphone capture, VAD capture, hotkey, tray control, Web UI, or text
  insertion is implemented here.
- The prototype's `--download` is an explicit developer action, not automatic
  installation. The product runtime still has no download path.
- `lang`, `emotion`, and `event` are model output tags, not user intent or
  application state.
- No claim about accuracy across languages, accents, noise, or meeting audio;
  the measured run is one synthetic zh-CN utterance.
- This work is independent of the app-shortcut work in PR #36 and does not
  describe or imply its behaviour.

## Open questions

- Whether the native helper links `sherpa-onnx` directly or bundles a small
  prebuilt helper, and how that is packaged per platform and per CPU baseline.
- Whether discovery should require a recorded hash when the DSH cache changes,
  without breaking the shared-cache use case.
- Exact focus verification and insertion method per target application, and how
  to report an unverified dispatch honestly.
- Memory and latency of a resident model versus loading per utterance; the
  prototype loads per process and has not been measured as a service.
- License review for redistributing weights versus download-on-demand, and the
  update/consent flow for a newer pinned model.

