# SenseVoice transcription prototype (dev only)

This directory contains a development prototype for
[#44](https://github.com/loonghao/vibdock/issues/44). It is **not** a runtime
dependency of `vibdock.exe`, is not built or packaged by CI, and does not add
speech-to-text support to VibDock. It exists to prove that a local SenseVoice
ONNX model can transcribe a WAV offline before any native helper is designed.

The product contract, discovery rules, privacy expectations, and planned
milestones are in [Local speech input](../../docs/design/local-speech-input.md).

## Requirements

- Python 3.10 or newer (validated with CPython 3.12).
- The prototype dependencies:

  ```powershell
  python -m pip install "sherpa-onnx==1.13.8" numpy scipy
  ```

  Validated with `sherpa-onnx` 1.13.8, `numpy` 2.5.1, and `scipy` 1.18.0. Newer
  versions are untested.
- A local SenseVoice ONNX model, discovered automatically or installed with the
  explicit download described below.

## Model discovery

The script searches in this order:

1. the existing DeepSeek Harness cache, treated as read-only:
   `%USERPROFILE%\.dsh\speech-to-text\sensevoice\models\sensevoice-onnx`
   (`%DSH_HOME%` replaces `%USERPROFILE%\.dsh` when set);
2. the VibDock-owned directory: `%LOCALAPPDATA%\VibDock\models\sensevoice`.

A directory is usable when it contains `tokens.txt` and either
`model.int8.onnx` or `model.onnx`, all non-empty. `--model-dir` overrides
discovery and fails closed: a path that is not a usable model is an error, not a
reason to try a different model. Nothing is downloaded during discovery.

## Usage

```powershell
python scripts/stt/sensevoice_transcribe.py C:\path\to\sample.wav
```

Real run on the development host, using the discovered DSH cache and a 5.832 s
Windows SAPI zh-CN WAV (22.05 kHz mono 16-bit PCM, resampled in memory to
16 kHz), CPU, two threads:

```json
{"text": "项目速记整理本周的工作要点，记录接下来的安排。", "lang": "zh", "emotion": "NEUTRAL", "event": "Speech", "audio_seconds": 5.832, "seconds": 0.128}
```

The same WAV transcribed with a freshly downloaded model in an explicit
directory took 0.1592 s and produced the identical text. Model load happens per
process and is not included in `seconds` (it was roughly 1.6 s in these runs).

Options:

- `--model-dir <dir>` — explicit model directory, hard override;
- `--threads <n>` — inference threads (default 2);
- `--language auto|zh|en|ja|ko|yue` — SenseVoice language hint (default `auto`);
- `--no-itn` — disable inverse text normalization;
- `--download --consent-download` — explicit, checksum-verified install.

`seconds` is the decode call only. `audio_seconds` is the source duration.
SenseVoice's `<|zh|>`-style tags are returned without the tag markers.

## Explicit model download

There is no automatic install. The download is a separate, deliberate action
that requires the consent flag:

```powershell
python scripts/stt/sensevoice_transcribe.py --download --consent-download
```

The install target defaults to `%LOCALAPPDATA%\VibDock\models\sensevoice` and
can be changed with `--model-dir`. Pinned details:

- source: `https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17.tar.bz2`
- size: 163,002,883 bytes;
- SHA-256: `7d1efa2138a65b0b488df37f8b89e3d91a60676e416f515b952358d83dfd347e`
  (verified again after download; a mismatch aborts and discards the archive);
- HTTPS only; a redirect away from HTTPS aborts;
- license: the upstream SenseVoice repository declares MIT. Review
  <https://github.com/FunAudioLLM/SenseVoice/blob/main/LICENSE> and the model
  reference <https://k2-fsa.github.io/sherpa/onnx/sense-voice/pretrained.html>
  before passing `--consent-download`.

Only `model.int8.onnx` and `tokens.txt` are installed from the verified archive.
The archive and its extraction directory are temporary and are removed
afterwards. `--download` without `--consent-download` exits 2 without creating
anything; running it again against an installed directory reports
`already_present` and does not re-download.

The verified install path was exercised end to end on the development host:
download, size check, SHA-256 check, extraction, and a successful transcription.

## Missing model

With no model and empty discovery locations the script exits **4** and prints
the searched paths plus the two installation routes:

```text
error: no usable local SenseVoice model was found.

Looked for model.int8.onnx (or model.onnx) plus tokens.txt in:
  - %USERPROFILE%\.dsh\speech-to-text\sensevoice\models\sensevoice-onnx (DSH SenseVoice cache)
  - %LOCALAPPDATA%\VibDock\models\sensevoice (VibDock model directory)

Options:
  1. Use a model that is already on this machine:
       python scripts/stt/sensevoice_transcribe.py <wav> --model-dir <dir>
  2. Install the pinned INT8 model explicitly over HTTPS, then verify it:
       python scripts/stt/sensevoice_transcribe.py --download --consent-download
     ...
No model is ever downloaded silently.
```

An explicit empty `--model-dir` also exits **4**, naming the checked directory
and the expected files.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | transcription (or explicit download) succeeded |
| 1 | WAV unreadable, inference failed, or download/verification failed |
| 2 | invalid arguments, including a download without consent |
| 3 | a required Python dependency is missing |
| 4 | no usable model in discovery or in the explicit directory |

## Privacy and limits

- Audio is read from the WAV file, resampled in memory, and decoded locally with
  `sherpa-onnx`. The script makes no network request except an explicit,
  consented `--download`; it never uploads audio.
- The transcript is printed to stdout as JSON because that is the prototype's
  output. The script writes no transcript or audio to a log, and it creates no
  temporary audio file.
- WAV input must be PCM readable by Python's `wave` module (8/16/24/32-bit PCM);
  compressed or floating-point WAV files are unsupported. Input is downmixed
  and resampled to 16 kHz mono.
- One utterance per process: no streaming, partial results, VAD segmentation,
  microphone capture, hotkey, or text insertion. This prototype does not
  transcribe from a microphone and never types into another application.
- Transcription is not verified voice-session state; see the design document's
  explicit non-claims.

