//! Optional native local speech-to-text using the SenseVoice ONNX model.
//!
//! This is the first runtime milestone: a bounded WAV-to-text command. It does
//! not capture a microphone, manage a hotkey, or insert text into another
//! window. Model discovery is local-only and read-only; downloading is a
//! separate, consent-gated developer tool (`scripts/stt`), never a startup
//! side effect of this command.

use serde_json::json;
use sherpa_onnx::{
    OfflineRecognizer, OfflineRecognizerConfig, OfflineSenseVoiceModelConfig, SileroVadModelConfig,
    TenVadModelConfig, VadModelConfig, VoiceActivityDetector, Wave,
};
use std::{
    env,
    path::{Path, PathBuf},
};

const MODEL_NAMES: [&str; 2] = ["model.int8.onnx", "model.onnx"];

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ModelPaths {
    pub(crate) model: PathBuf,
    pub(crate) tokens: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
struct Cli {
    wav: PathBuf,
    model_dir: Option<PathBuf>,
    json: bool,
}

fn parse_args(args: &[String]) -> std::result::Result<Cli, String> {
    let mut wav = None;
    let mut model_dir = None;
    let mut json = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--wav" => {
                let value = args.get(index + 1).filter(|value| !value.is_empty());
                let Some(value) = value else {
                    return Err("--wav requires a WAV file path.".into());
                };
                if wav.replace(PathBuf::from(value)).is_some() {
                    return Err("--wav may be specified only once.".into());
                }
                index += 2;
            }
            "--model-dir" => {
                let value = args.get(index + 1).filter(|value| !value.is_empty());
                let Some(value) = value else {
                    return Err("--model-dir requires a directory path.".into());
                };
                if model_dir.replace(PathBuf::from(value)).is_some() {
                    return Err("--model-dir may be specified only once.".into());
                }
                index += 2;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            other => return Err(format!("Unknown transcribe argument: {other}")),
        }
    }
    let Some(wav) = wav else {
        return Err("Specify a mono PCM WAV with --wav <path>.".into());
    };
    Ok(Cli {
        wav,
        model_dir,
        json,
    })
}

fn valid_model_dir(dir: &Path) -> Option<ModelPaths> {
    if !dir.is_dir() {
        return None;
    }
    let tokens = dir.join("tokens.txt");
    if !tokens.is_file() || tokens.metadata().ok()?.len() == 0 {
        return None;
    }
    let model = MODEL_NAMES
        .iter()
        .map(|name| dir.join(name))
        .find(|path| path.is_file() && path.metadata().is_ok_and(|meta| meta.len() > 0))?;
    Some(ModelPaths { model, tokens })
}

/// Resolves a local model without network access.
///
/// Priority: explicit `--model-dir`, `VIBDOCK_STT_MODEL_DIR`, VibDock's own
/// model directory, then the existing DeepSeek Harness SenseVoice cache. The
/// DSH cache is read, never modified.
pub(crate) fn discover_model(explicit: Option<&Path>) -> std::result::Result<ModelPaths, String> {
    if let Some(dir) = explicit {
        return valid_model_dir(dir).ok_or_else(|| {
            format!(
                "{} does not contain tokens.txt and a SenseVoice model.int8.onnx/model.onnx.",
                dir.display()
            )
        });
    }
    let mut candidates = Vec::new();
    if let Some(dir) = env::var_os("VIBDOCK_STT_MODEL_DIR") {
        candidates.push(PathBuf::from(dir));
    }
    if let Some(home) = env::var_os("USERPROFILE").or_else(|| env::var_os("HOME")) {
        candidates.push(
            PathBuf::from(home)
                .join(".dsh")
                .join("speech-to-text")
                .join("sensevoice")
                .join("models")
                .join("sensevoice-onnx"),
        );
    }
    if let Some(local) = env::var_os("LOCALAPPDATA") {
        candidates.push(
            PathBuf::from(local)
                .join("VibDock")
                .join("models")
                .join("sensevoice"),
        );
    }
    for candidate in &candidates {
        if let Some(paths) = valid_model_dir(candidate) {
            return Ok(paths);
        }
    }
    Err(format!(
        "No local SenseVoice model found. Checked: {}. Install one with `python scripts/stt/sensevoice_transcribe.py --download --consent-download`, or pass --model-dir.",
        candidates
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

fn load_wave(path: &Path) -> std::result::Result<Wave, String> {
    let text = path
        .to_str()
        .ok_or_else(|| "WAV path is not valid UTF-8.".to_string())?;
    Wave::read(text).ok_or_else(|| {
        format!(
            "{} could not be read as a mono PCM WAV. Convert it to 16-bit mono PCM first.",
            path.display()
        )
    })
}

pub(crate) fn transcribe_wav(
    wav: &Path,
    model: &ModelPaths,
) -> std::result::Result<(String, f32), String> {
    let wave = load_wave(wav)?;
    recognize(wave.samples(), wave.sample_rate(), model)
}

pub(crate) fn wav_seconds(wav: &Path) -> std::result::Result<f32, String> {
    let wave = load_wave(wav)?;
    if wave.sample_rate() <= 0 {
        return Err("The WAV has no usable sample rate.".into());
    }
    Ok(wave.samples().len() as f32 / wave.sample_rate() as f32)
}

/// Resolves the local Silero VAD model without network access.
pub(crate) fn discover_vad_model(explicit: Option<&Path>) -> std::result::Result<PathBuf, String> {
    if let Some(path) = explicit {
        return valid_vad(path).ok_or_else(|| {
            format!(
                "{} is not a non-empty silero_vad.onnx file.",
                path.display()
            )
        });
    }
    let mut candidates = Vec::new();
    if let Some(path) = env::var_os("VIBDOCK_STT_VAD_MODEL") {
        candidates.push(PathBuf::from(path));
    }
    if let Some(home) = env::var_os("USERPROFILE").or_else(|| env::var_os("HOME")) {
        candidates.push(
            PathBuf::from(home)
                .join(".dsh")
                .join("speech-to-text")
                .join("sensevoice")
                .join("models")
                .join("silero")
                .join("silero_vad.onnx"),
        );
    }
    if let Some(local) = env::var_os("LOCALAPPDATA") {
        candidates.push(
            PathBuf::from(local)
                .join("VibDock")
                .join("models")
                .join("silero_vad.onnx"),
        );
    }
    for candidate in &candidates {
        if let Some(path) = valid_vad(candidate) {
            return Ok(path);
        }
    }
    Err(format!(
        "No local Silero VAD model found. Checked: {}. Set VIBDOCK_STT_VAD_MODEL or pass --vad-model.",
        candidates
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

fn valid_vad(path: &Path) -> Option<PathBuf> {
    (path.is_file() && path.metadata().is_ok_and(|meta| meta.len() > 0)).then(|| path.to_path_buf())
}

/// Trims a 16 kHz mono clip to Silero-detected speech. An empty result is a
/// distinct "no speech" failure, never an empty transcript.
pub(crate) fn vad_speech(samples: &[f32], model: &Path) -> std::result::Result<Vec<f32>, String> {
    if samples.is_empty() {
        return Err("There is no audio to segment.".into());
    }
    let config = VadModelConfig {
        silero_vad: SileroVadModelConfig {
            model: Some(model.to_string_lossy().into_owned()),
            threshold: 0.5,
            min_silence_duration: 0.5,
            min_speech_duration: 0.25,
            window_size: 512,
            max_speech_duration: 30.0,
        },
        ten_vad: TenVadModelConfig {
            model: None,
            threshold: 0.5,
            min_silence_duration: 0.5,
            min_speech_duration: 0.25,
            window_size: 256,
            max_speech_duration: 30.0,
        },
        sample_rate: 16_000,
        num_threads: 1,
        provider: Some("cpu".into()),
        debug: false,
    };
    let detector = VoiceActivityDetector::create(&config, 30.0)
        .ok_or_else(|| "Silero VAD failed to load.".to_string())?;
    detector.accept_waveform(samples);
    detector.flush();
    let mut speech = Vec::new();
    while let Some(segment) = detector.front() {
        speech.extend_from_slice(segment.samples());
        detector.pop();
    }
    if speech.is_empty() {
        return Err("Silero VAD detected no speech.".into());
    }
    Ok(speech)
}

/// Transcribes an in-memory mono signal. Used by dictation capture and tests;
/// the sample rate may be any positive rate the model's frontend accepts.
pub(crate) fn transcribe_samples(
    samples: &[f32],
    sample_rate: i32,
    model: &ModelPaths,
) -> std::result::Result<(String, f32), String> {
    if samples.is_empty() {
        return Err("No audio samples were captured.".into());
    }
    recognize(samples, sample_rate, model)
}

fn recognize(
    samples: &[f32],
    sample_rate: i32,
    model: &ModelPaths,
) -> std::result::Result<(String, f32), String> {
    let mut config = OfflineRecognizerConfig::default();
    config.model_config.sense_voice = OfflineSenseVoiceModelConfig {
        model: Some(model.model.to_string_lossy().into_owned()),
        language: Some("auto".into()),
        use_itn: true,
    };
    config.model_config.tokens = Some(model.tokens.to_string_lossy().into_owned());
    let recognizer = OfflineRecognizer::create(&config)
        .ok_or_else(|| "SenseVoice model failed to load.".to_string())?;
    let stream = recognizer.create_stream();
    stream.accept_waveform(sample_rate, samples);
    let started = std::time::Instant::now();
    recognizer.decode(&stream);
    let result = stream
        .get_result()
        .ok_or_else(|| "SenseVoice returned no result.".to_string())?;
    let seconds = started.elapsed().as_secs_f32();
    Ok((result.text.trim().to_string(), seconds))
}

/// Runs `vibdock transcribe`. Only this command invokes the native model.
pub(crate) fn run(args: &[String]) -> crate::Result<()> {
    let cli = parse_args(args)?;
    let model = discover_model(cli.model_dir.as_deref())?;
    let (text, seconds) = transcribe_wav(&cli.wav, &model)?;
    if cli.json {
        println!(
            "{}",
            json!({
                "text": text,
                "seconds": seconds,
                "model": model.model.display().to_string(),
            })
        );
    } else {
        println!("{text}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn transcribe_requires_a_wav_and_rejects_unknown_arguments() {
        assert!(parse_args(&args(&[])).is_err());
        assert!(parse_args(&args(&["--json"])).is_err());
        assert!(parse_args(&args(&["--wav", ""])).is_err());
        assert!(parse_args(&args(&["--wav", "a.wav", "--wav", "b.wav"])).is_err());
        assert!(parse_args(&args(&["--wav", "a.wav", "--bogus"])).is_err());
        assert_eq!(
            parse_args(&args(&["--wav", "a.wav", "--json"])).unwrap(),
            Cli {
                wav: PathBuf::from("a.wav"),
                model_dir: None,
                json: true,
            }
        );
    }

    #[test]
    fn explicit_model_discovery_is_local_and_fails_closed() {
        let dir = env::temp_dir().join(format!(
            "vibdock-stt-model-test-{}",
            crate::service::generate_token().unwrap()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(discover_model(Some(&dir)).is_err());
        std::fs::write(dir.join("tokens.txt"), "token\n").unwrap();
        assert!(discover_model(Some(&dir)).is_err());
        std::fs::write(dir.join("model.int8.onnx"), "not a real model").unwrap();
        let paths = discover_model(Some(&dir)).unwrap();
        assert_eq!(paths.tokens, dir.join("tokens.txt"));
        assert_eq!(paths.model, dir.join("model.int8.onnx"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
