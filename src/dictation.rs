//! Local hold-to-talk-style dictation: bounded microphone capture, SenseVoice
//! transcription, and optional Unicode insertion into the focused window.
//!
//! This is deliberately not a background listener: every capture is started by
//! an explicit command, stops after a bounded duration, and never sends Enter.
//! The transcript and audio stay local.

use crate::{
    hotkey::Hotkey,
    stt,
    windows::{escape_is_down, poll_hotkeys, trigger_is_down, RegisteredHotkey},
};
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    FromSample, Sample, SampleFormat, StreamConfig,
};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const TARGET_RATE: u32 = 16_000;
const MAX_SECONDS: u32 = 30;
const HOLD_HOTKEY_ID: i32 = 9001;

struct Cli {
    seconds: u32,
    wav: Option<PathBuf>,
    hold: bool,
    trigger: Option<String>,
    insert: bool,
    target: Option<String>,
    model_dir: Option<PathBuf>,
    json: bool,
}

fn parse_args(args: &[String]) -> std::result::Result<Cli, String> {
    let mut seconds = 5u32;
    let mut seconds_set = false;
    let mut wav = None;
    let mut hold = false;
    let mut trigger = None;
    let mut insert = false;
    let mut target = None;
    let mut model_dir = None;
    let mut json = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--seconds" => {
                let value = args
                    .get(index + 1)
                    .and_then(|value| value.parse::<u32>().ok())
                    .filter(|value| (1..=MAX_SECONDS).contains(value))
                    .ok_or_else(|| format!("--seconds must be between 1 and {MAX_SECONDS}."))?;
                if seconds_set {
                    return Err("--seconds may be specified only once.".into());
                }
                seconds = value;
                seconds_set = true;
                index += 2;
            }
            "--hold" => {
                hold = true;
                index += 1;
            }
            "--trigger" => {
                let value = args
                    .get(index + 1)
                    .filter(|value| !value.is_empty() && !value.contains('\0'))
                    .ok_or_else(|| "--trigger requires a shortcut.".to_string())?;
                if trigger.replace(value.clone()).is_some() {
                    return Err("--trigger may be specified only once.".into());
                }
                index += 2;
            }
            "--wav" => {
                let value = args
                    .get(index + 1)
                    .filter(|value| !value.is_empty() && !value.contains('\0'))
                    .ok_or_else(|| "--wav requires a WAV file path.".to_string())?;
                if wav.replace(PathBuf::from(value)).is_some() {
                    return Err("--wav may be specified only once.".into());
                }
                index += 2;
            }
            "--model-dir" => {
                let value = args
                    .get(index + 1)
                    .filter(|value| !value.is_empty() && !value.contains('\0'))
                    .ok_or_else(|| "--model-dir requires a directory path.".to_string())?;
                if model_dir.replace(PathBuf::from(value)).is_some() {
                    return Err("--model-dir may be specified only once.".into());
                }
                index += 2;
            }
            "--insert" => {
                insert = true;
                index += 1;
            }
            "--target" => {
                let value = args
                    .get(index + 1)
                    .filter(|value| !value.trim().is_empty() && !value.contains('\0'))
                    .ok_or_else(|| "--target requires a window-title substring.".to_string())?;
                if target.replace(value.clone()).is_some() {
                    return Err("--target may be specified only once.".into());
                }
                index += 2;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            other => return Err(format!("Unknown dictate argument: {other}")),
        }
    }
    if wav.is_some() && seconds_set {
        return Err("Use either --wav or --seconds, not both.".into());
    }
    if hold {
        if wav.is_some() {
            return Err("--hold cannot be combined with --wav.".into());
        }
        let Some(trigger) = trigger.as_deref() else {
            return Err("--hold requires --trigger <chord>.".into());
        };
        Hotkey::parse(trigger).map_err(|error| error.to_string())?;
        if !seconds_set {
            seconds = MAX_SECONDS;
        }
    } else if trigger.is_some() {
        return Err("--trigger requires --hold.".into());
    }
    if target.is_some() && !insert {
        return Err("--target requires --insert.".into());
    }
    Ok(Cli {
        seconds,
        wav,
        hold,
        trigger,
        insert,
        target,
        model_dir,
        json,
    })
}

fn append<T>(data: &[T], buffer: &Arc<Mutex<Vec<f32>>>)
where
    T: Sample,
    f32: FromSample<T>,
{
    let mut buffer = buffer.lock().expect("capture buffer poisoned");
    buffer.extend(data.iter().map(|sample| sample.to_sample::<f32>()));
}

fn record(seconds: u32) -> std::result::Result<(Vec<f32>, u32), String> {
    record_until(seconds, || false)
}

fn record_until<F>(max_seconds: u32, stop: F) -> std::result::Result<(Vec<f32>, u32), String>
where
    F: Fn() -> bool,
{
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| "No default microphone is available.".to_string())?;
    let supported = device.default_input_config().map_err(|error| {
        format!("The default microphone has no usable input configuration: {error}")
    })?;
    let format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let sample_rate = config.sample_rate.0;
    let channels = config.channels as usize;
    let buffer = Arc::new(Mutex::new(Vec::<f32>::new()));
    let captured = Arc::clone(&buffer);
    let error_slot = Arc::new(Mutex::new(None::<String>));
    let error_callback = {
        let slot = Arc::clone(&error_slot);
        move |error: cpal::StreamError| {
            if let Ok(mut slot) = slot.lock() {
                *slot = Some(error.to_string());
            }
        }
    };
    macro_rules! stream {
        ($sample:ty) => {{
            let captured = Arc::clone(&captured);
            device.build_input_stream(
                &config,
                move |data: &[$sample], _| append::<$sample>(data, &captured),
                error_callback,
                None,
            )
        }};
    }
    let stream = match format {
        SampleFormat::F32 => stream!(f32),
        SampleFormat::F64 => stream!(f64),
        SampleFormat::I16 => stream!(i16),
        SampleFormat::U16 => stream!(u16),
        SampleFormat::I32 => stream!(i32),
        SampleFormat::U32 => stream!(u32),
        SampleFormat::I8 => stream!(i8),
        SampleFormat::U8 => stream!(u8),
        other => {
            return Err(format!(
                "The default microphone uses an unsupported sample format: {other:?}"
            ))
        }
    }
    .map_err(|error| format!("The default microphone could not be opened: {error}"))?;
    stream
        .play()
        .map_err(|error| format!("The microphone could not be started: {error}"))?;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(max_seconds as u64) && !stop() {
        std::thread::sleep(Duration::from_millis(40));
    }
    drop(stream);
    if let Ok(slot) = error_slot.lock() {
        if let Some(error) = slot.as_ref() {
            return Err(format!("Microphone capture failed: {error}"));
        }
    }
    let interleaved = buffer
        .lock()
        .map_err(|_| "Microphone capture buffer is unavailable.".to_string())?
        .clone();
    if interleaved.is_empty() {
        return Err("No microphone audio was captured.".into());
    }
    if channels == 0 {
        return Err("The microphone reported no channels.".into());
    }
    let mono = interleaved
        .chunks(channels)
        .map(|frame| frame.iter().copied().sum::<f32>() / channels as f32)
        .collect::<Vec<_>>();
    let peak = mono
        .iter()
        .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    if peak < 1e-5 {
        return Err("The microphone captured only silence.".into());
    }
    Ok((resample(&mono, sample_rate, TARGET_RATE), sample_rate))
}

/// Linear resampling is sufficient for a bounded dictation snippet and keeps
/// this path dependency-free; the model frontend still receives 16 kHz mono.
fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let ratio = from as f64 / to as f64;
    let output_len = ((input.len() as f64) / ratio).round() as usize;
    (0..output_len)
        .map(|index| {
            let position = index as f64 * ratio;
            let left = position.floor() as usize;
            let fraction = (position - left as f64) as f32;
            let left_sample = input.get(left).copied().unwrap_or(0.0);
            let right_sample = input.get(left + 1).copied().unwrap_or(left_sample);
            left_sample + (right_sample - left_sample) * fraction
        })
        .collect()
}

fn hold(cli: &Cli, model: &stt::ModelPaths) -> std::result::Result<(String, f32, f32), String> {
    let trigger = cli
        .trigger
        .as_deref()
        .ok_or_else(|| "--hold requires --trigger <chord>.".to_string())?;
    let hotkey = Hotkey::parse(trigger).map_err(|error| error.to_string())?;
    let _registration = RegisteredHotkey::register(&hotkey, HOLD_HOTKEY_ID)?;
    eprintln!("Hold {trigger} and speak; release to stop, Esc to cancel.");
    loop {
        if poll_hotkeys().contains(&HOLD_HOTKEY_ID) {
            break;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    let (samples, _sample_rate) = record_until(cli.seconds, || {
        !trigger_is_down(&hotkey).unwrap_or(false) || escape_is_down()
    })?;
    if escape_is_down() {
        return Err("Cancelled; the captured audio was discarded.".into());
    }
    let audio_seconds = samples.len() as f32 / TARGET_RATE as f32;
    let (text, seconds) = stt::transcribe_samples(&samples, TARGET_RATE as i32, model)?;
    Ok((text, audio_seconds, seconds))
}

/// Runs `vibdock dictate`.
pub(crate) fn run(args: &[String]) -> crate::Result<()> {
    let cli = parse_args(args)?;
    let model = stt::discover_model(cli.model_dir.as_deref())?;
    let (text, audio_seconds, seconds) = if cli.hold {
        hold(&cli, &model)?
    } else if let Some(wav) = &cli.wav {
        let (text, seconds) = stt::transcribe_wav(wav, &model)?;
        (text, stt::wav_seconds(wav)?, seconds)
    } else {
        let (samples, _sample_rate) = record(cli.seconds)?;
        let audio_seconds = samples.len() as f32 / TARGET_RATE as f32;
        let (text, seconds) = stt::transcribe_samples(&samples, TARGET_RATE as i32, &model)?;
        (text, audio_seconds, seconds)
    };
    if text.is_empty() {
        return Err("The model returned an empty transcript.".into());
    }
    let inserted = if cli.insert {
        crate::windows::insert_text(&text, cli.target.as_deref())?;
        true
    } else {
        false
    };
    if cli.json {
        println!(
            "{}",
            json!({
                "text": text,
                "audio_seconds": audio_seconds,
                "seconds": seconds,
                "inserted": inserted,
                "model": model.model.display().to_string(),
            })
        );
    } else {
        println!("{text}");
        if inserted {
            println!("Inserted the transcript into the foreground window; content is unverified.");
        }
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
    fn dictate_bounds_duration_and_rejects_conflicting_inputs() {
        assert!(parse_args(&args(&["--seconds"])).is_err());
        assert!(parse_args(&args(&["--seconds", "0"])).is_err());
        assert!(parse_args(&args(&["--seconds", "31"])).is_err());
        assert!(parse_args(&args(&["--seconds", "2", "--seconds", "3"])).is_err());
        assert!(parse_args(&args(&["--seconds", "2", "--wav", "a.wav"])).is_err());
        assert!(parse_args(&args(&["--bogus"])).is_err());
        let cli = parse_args(&args(&["--seconds", "2", "--insert", "--json"])).unwrap();
        assert_eq!(cli.seconds, 2);
        assert!(cli.insert && cli.json);
        assert!(parse_args(&args(&["--target", "Notepad"])).is_err());
        let targeted = parse_args(&args(&[
            "--wav", "a.wav", "--insert", "--target", "Notepad",
        ]))
        .unwrap();
        assert_eq!(targeted.target.as_deref(), Some("Notepad"));
    }

    #[test]
    fn hold_requires_a_valid_trigger_and_rejects_wav() {
        assert!(parse_args(&args(&["--hold"])).is_err());
        assert!(parse_args(&args(&[
            "--hold",
            "--trigger",
            "Ctrl+Alt+Space",
            "--wav",
            "a.wav"
        ]))
        .is_err());
        assert!(parse_args(&args(&["--trigger", "Ctrl+Alt+Space"])).is_err());
        assert!(parse_args(&args(&["--hold", "--trigger", "not a chord"])).is_err());
        let cli = parse_args(&args(&["--hold", "--trigger", "Ctrl+Alt+Space"])).unwrap();
        assert!(cli.hold);
        assert_eq!(cli.seconds, MAX_SECONDS);
        assert_eq!(cli.trigger.as_deref(), Some("Ctrl+Alt+Space"));
    }

    #[test]
    fn resampling_preserves_duration_and_rejects_short_input() {
        let input = vec![0.0f32; 48_000];
        assert_eq!(resample(&input, 48_000, 16_000).len(), 16_000);
        assert_eq!(resample(&[], 48_000, 16_000).len(), 0);
        let ramp = (0..6).map(|value| value as f32).collect::<Vec<_>>();
        let out = resample(&ramp, 6, 3);
        assert_eq!(out.len(), 3);
    }
}
