//! Exact-endpoint observations and explicit short tests, only in disposable helpers.
use crate::resident_apps::audio_probe;
use serde_json::{json, Value};
use std::{
    ptr,
    time::{Duration, Instant},
};
use windows::{
    core::{Interface, HRESULT, PCWSTR},
    Win32::{
        Foundation::ERROR_NOT_FOUND,
        Media::Audio::{
            eCapture,
            Endpoints::{IAudioEndpointVolume, IAudioMeterInformation},
            IAudioCaptureClient, IAudioClient, IMMDevice, IMMDeviceEnumerator, IMMEndpoint,
            MMDeviceEnumerator, AUDCLNT_E_DEVICE_IN_USE, AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_NOPERSIST, DEVICE_STATE_ACTIVE, ENDPOINT_HARDWARE_SUPPORT_METER,
            WAVEFORMATEX,
        },
        System::Com::{
            CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
            COINIT_APARTMENTTHREADED,
        },
    },
};

const TEST_DURATION: Duration = Duration::from_millis(1500);

/// Exact capture-endpoint writes live only in this disposable helper. A successful
/// COM write is insufficient: the same endpoint must return the desired mute.
pub(super) fn action(id: &str, operation: &str) -> Value {
    let mut value =
        audio_probe::action_failure(id, operation, false, "microphone_action_unavailable");
    unsafe {
        let result = (|| -> Result<(), String> {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED)
                .ok()
                .map_err(|e| error_code("com_unavailable", e))?;
            let _com = Com;
            let manager: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .map_err(|e| error_code("microphone_enumerator_unavailable", e))?;
            let device = capture_device(&manager, id).map_err(|(_, error)| error)?;
            let volume: IAudioEndpointVolume = device
                .Activate(CLSCTX_ALL, None)
                .map_err(|e| error_code("microphone_volume_unavailable", e))?;
            let before = volume
                .GetMute()
                .map_err(|e| error_code("microphone_mute_unavailable", e))?
                .as_bool();
            value["before_muted"] = json!(before);
            let desired = match operation {
                "mute" => true,
                "unmute" => false,
                "toggle" => !before,
                _ => return Err("unknown_microphone_operation".into()),
            };
            capture_device(&manager, id).map_err(|(_, error)| error)?;
            if desired != before {
                value["write_attempted"] = json!(true);
                volume
                    .SetMute(desired, ptr::null())
                    .map_err(|e| error_code("microphone_write_failed", e))?;
            }
            capture_device(&manager, id).map_err(|(_, error)| error)?;
            let after = volume
                .GetMute()
                .map_err(|e| error_code("microphone_readback_failed", e))?
                .as_bool();
            value["muted"] = json!(after);
            if after != desired {
                return Err("microphone_readback_mismatch".into());
            }
            value["state"] = json!("verified");
            value["error"] = Value::Null;
            Ok(())
        })();
        if let Err(error) = result {
            value["state"] = json!(if value["write_attempted"] == true {
                "outcome_unconfirmed"
            } else {
                "rejected_before_write"
            });
            value["error"] = json!(error);
        }
    }
    value
}

struct Com;
impl Drop for Com {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}
struct MixFormat(*mut WAVEFORMATEX);
impl Drop for MixFormat {
    fn drop(&mut self) {
        unsafe { CoTaskMemFree(Some(self.0.cast())) };
    }
}
struct Capture {
    client: IAudioClient,
    running: bool,
}
impl Drop for Capture {
    fn drop(&mut self) {
        if self.running {
            unsafe {
                let _ = self.client.Stop();
            }
        }
    }
}

fn error_code(stage: &str, error: windows::core::Error) -> String {
    format!("{stage} (0x{:08X})", error.code().0 as u32)
}

fn fail(value: &mut Value, state: &str, error: &str) {
    value["state"] = json!(state);
    value["error"] = json!(error);
    value["peak"] = Value::Null;
    value["level_state"] = json!("unknown");
}

unsafe fn capture_device(
    manager: &IMMDeviceEnumerator,
    id: &str,
) -> Result<IMMDevice, (&'static str, String)> {
    let wide: Vec<u16> = id.encode_utf16().chain(Some(0)).collect();
    let device = manager.GetDevice(PCWSTR(wide.as_ptr())).map_err(|error| {
        let state = if error.code() == HRESULT::from_win32(ERROR_NOT_FOUND.0) {
            "disconnected"
        } else {
            "unknown"
        };
        (state, error_code("microphone_open_failed", error))
    })?;
    if crate::windows::device_id(&device).ok().as_deref() != Some(id) {
        return Err(("unknown", "microphone_identity_changed".into()));
    }
    let endpoint: IMMEndpoint = device
        .cast()
        .map_err(|error| ("unknown", error_code("microphone_flow_unavailable", error)))?;
    if endpoint
        .GetDataFlow()
        .map_err(|error| ("unknown", error_code("microphone_flow_unavailable", error)))?
        != eCapture
    {
        return Err(("unknown", "selected_endpoint_is_not_a_microphone".into()));
    }
    if device
        .GetState()
        .map_err(|error| ("unknown", error_code("microphone_state_unavailable", error)))?
        != DEVICE_STATE_ACTIVE
    {
        return Err((
            "disconnected",
            "selected_microphone_is_disconnected_or_disabled".into(),
        ));
    }
    Ok(device)
}

unsafe fn read_mute(device: &IMMDevice, value: &mut Value) -> Result<(), String> {
    let volume: IAudioEndpointVolume = device
        .Activate(CLSCTX_ALL, None)
        .map_err(|error| error_code("microphone_volume_unavailable", error))?;
    let muted = volume
        .GetMute()
        .map_err(|error| error_code("microphone_mute_unavailable", error))?
        .as_bool();
    value["muted"] = json!(muted);
    value["state"] = json!(if muted { "muted" } else { "unmuted" });
    value["error"] = Value::Null;
    Ok(())
}

unsafe fn shared_capture(device: &IMMDevice) -> Result<(Capture, IAudioCaptureClient), String> {
    let client: IAudioClient = device
        .Activate(CLSCTX_ALL, None)
        .map_err(|error| error_code("capture_client_unavailable", error))?;
    let format = MixFormat(
        client
            .GetMixFormat()
            .map_err(|error| error_code("capture_format_unavailable", error))?,
    );
    if format.0.is_null() {
        return Err("capture_format_unavailable".into());
    }
    client
        .Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_NOPERSIST,
            1_000_000,
            0,
            format.0,
            None,
        )
        .map_err(|error| {
            if error.code() == AUDCLNT_E_DEVICE_IN_USE {
                "shared_capture_unavailable_device_in_use; exclusive_software_meter_is_unknown"
                    .into()
            } else {
                error_code("shared_capture_initialize_failed", error)
            }
        })?;
    let packets = client
        .GetService::<IAudioCaptureClient>()
        .map_err(|error| error_code("capture_packets_unavailable", error))?;
    client
        .Start()
        .map_err(|error| error_code("shared_capture_start_failed", error))?;
    Ok((
        Capture {
            client,
            running: true,
        },
        packets,
    ))
}

unsafe fn drain(packets: &IAudioCaptureClient, frames: &mut u64) -> Result<(), String> {
    for _ in 0..128 {
        if packets
            .GetNextPacketSize()
            .map_err(|error| error_code("capture_packet_size_failed", error))?
            == 0
        {
            return Ok(());
        }
        let mut data = ptr::null_mut();
        let mut count = 0;
        let mut flags = 0;
        packets
            .GetBuffer(&mut data, &mut count, &mut flags, None, None)
            .map_err(|error| error_code("capture_packet_failed", error))?;
        // Consume/discard frames without dereferencing or copying audio samples.
        packets
            .ReleaseBuffer(count)
            .map_err(|error| error_code("capture_release_failed", error))?;
        *frames = frames.saturating_add(u64::from(count));
    }
    Err("capture_packet_budget_exceeded".into())
}

pub(super) fn observe(id: Option<&str>, test: bool) -> Value {
    let mut value = audio_probe::unknown(id, "microphone_observation_unavailable");
    value["muted"] = Value::Null;
    value["age_ms"] = json!(0);
    if test {
        value["sample_count"] = json!(0);
        value["captured_frames"] = json!(0);
        value["signal_detected"] = Value::Null;
        value["duration_ms"] = json!(1500);
        value["elapsed_ms"] = json!(0);
    }
    let Some(id) = id else {
        value["state"] = json!("unconfigured");
        value["error"] = Value::Null;
        return value;
    };
    unsafe {
        if CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_err() {
            return value;
        }
        let _com = Com;
        let manager: IMMDeviceEnumerator =
            match CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) {
                Ok(manager) => manager,
                Err(error) => {
                    fail(
                        &mut value,
                        "unknown",
                        &error_code("microphone_enumerator_unavailable", error),
                    );
                    return value;
                }
            };
        let device = match capture_device(&manager, id) {
            Ok(device) => device,
            Err((state, error)) => {
                fail(&mut value, state, &error);
                return value;
            }
        };
        value["name"] = crate::windows::device_name(&device)
            .ok()
            .filter(|name| {
                !name.is_empty() && name.len() <= 160 && !name.chars().any(char::is_control)
            })
            .map_or(Value::Null, |name| json!(name));
        if let Err(error) = read_mute(&device, &mut value) {
            fail(&mut value, "unknown", &error);
            return value;
        }
        let meter: IAudioMeterInformation = match device.Activate(CLSCTX_ALL, None) {
            Ok(meter) => meter,
            Err(error) => {
                if test {
                    fail(
                        &mut value,
                        "unknown",
                        &error_code("microphone_meter_unavailable", error),
                    );
                }
                return value;
            }
        };
        let hardware = match meter.QueryHardwareSupport() {
            Ok(flags) => flags & ENDPOINT_HARDWARE_SUPPORT_METER != 0,
            Err(error) => {
                if test {
                    fail(
                        &mut value,
                        "unknown",
                        &error_code("microphone_meter_support_unknown", error),
                    );
                }
                return value;
            }
        };
        value["meter_source"] = json!(if hardware { "hardware" } else { "software" });
        if !test {
            // Software meters return an ambiguous zero during exclusive use.
            if let Some(peak) = meter
                .GetPeakValue()
                .ok()
                .and_then(|peak| audio_probe::passive_peak(peak, hardware))
            {
                value["peak"] = json!(peak);
                value["level_state"] = json!("available");
            }
            return value;
        }
        let (mut capture, packets) = match shared_capture(&device) {
            Ok(capture) => capture,
            Err(error) => {
                fail(&mut value, "unknown", &error);
                return value;
            }
        };
        let started = Instant::now();
        let mut peak = 0.0_f64;
        let mut count = 0_u64;
        let mut frames = 0_u64;
        let observed = (|| -> Result<(), String> {
            while started.elapsed() < TEST_DURATION {
                drain(&packets, &mut frames)?;
                let sample = meter
                    .GetPeakValue()
                    .map_err(|error| error_code("microphone_meter_read_failed", error))?;
                let sample =
                    audio_probe::finite_peak(sample).ok_or("microphone_meter_value_invalid")?;
                peak = peak.max(sample);
                count += 1;
                std::thread::sleep(Duration::from_millis(20));
            }
            drain(&packets, &mut frames)?;
            if frames == 0 {
                return Err("capture_no_frames_observed".into());
            }
            capture
                .client
                .Stop()
                .map_err(|error| error_code("shared_capture_stop_failed", error))?;
            capture.running = false;
            if device
                .GetState()
                .map_err(|error| error_code("microphone_state_unavailable", error))?
                != DEVICE_STATE_ACTIVE
            {
                return Err("microphone_disconnected_during_test".into());
            }
            read_mute(&device, &mut value)?;
            Ok(())
        })();
        value["elapsed_ms"] = json!(started.elapsed().as_millis().min(u64::MAX as u128) as u64);
        value["sample_count"] = json!(count);
        value["captured_frames"] = json!(frames);
        if let Err(error) = observed {
            fail(&mut value, "unknown", &error);
        } else {
            value["peak"] = json!(peak);
            value["level_state"] = json!("available");
            value["signal_detected"] = json!(peak > 0.001);
        }
        value
    }
}
