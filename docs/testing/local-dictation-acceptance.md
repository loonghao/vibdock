# Local dictation acceptance

Checked on 2026-10-18 against a debug build with `--features stt` on a Windows
11 x64 desktop. This is a dated single-host record, not a platform guarantee.

## Build

```powershell
cargo build --features stt
```

The build used the optional `sherpa-onnx` 1.13.8 and CPAL 0.15.3 dependencies.
The default preview build remains free of them.

## Real checks

1. **WAV transcription.** The 5.832 s Windows SAPI zh-CN WAV was transcribed
   with the existing DSH SenseVoice INT8 model:

   ```text
   {"audio_seconds":5.832,"inserted":false,"seconds":0.2363,
    "text":"项目速记整理本周的工作要点，记录接下来的安排。"}
   ```

2. **Microphone capture.** `dictate --seconds 2 --json` captured 2.013 s from
   the default input device and transcribed it locally. The ambient clip
   produced `I.`, which is not a meaningful accuracy result; it proves the
   capture/convert/recognize path ran and stayed local.

3. **Insertion.** With a real Notepad document focused, `dictate --wav <sample>
   --insert` reported `inserted=true` and UI Automation read back the exact
   transcript from the document:

   ```text
   项目速记整理本周的工作要点，记录接下来的安排。
   ```

   No Enter, Send button, form submission, clipboard change or network request
   was used.

   Because this is a terminal command, `--insert` targets whatever is focused
   when transcription finishes. The check focused Notepad explicitly before the
   command completed; a terminal-focused invocation would type into the
   terminal. A tray/hotkey target flow is still open.

4. **Fail-closed paths.** An out-of-range `--seconds`, a conflicting
   `--wav`/`--seconds`, an unknown flag, an unusable `--model-dir`, and a silent
   capture all returned an error before insertion.

## Limits

- No global hold-to-talk trigger, Silero VAD segmentation, configured-device
  selection, cancellation during recording, or target picker.
- Only one real insertion target (Notepad) was checked. Elevated, sandboxed and
  custom editors may reject injected Unicode.
- Success means input was sent, not that the application accepted, displayed or
  submitted it; no content or voice-session state is verified.
- No accuracy measurement across languages, accents, noise, or meeting audio.
- The native build is not a release artifact and has no packaging or license
  review for redistributed weights.
