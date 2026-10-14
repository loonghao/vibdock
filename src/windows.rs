use crate::{Action, Result};
use windows::{
    core::PCWSTR,
    Win32::{
        Devices::FunctionDiscovery::PKEY_Device_FriendlyName,
        Media::Audio::{Endpoints::IAudioEndpointVolume, *},
        System::Com::*,
        UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
    },
};

struct Com;
impl Drop for Com {
    fn drop(&mut self) {
        unsafe { CoUninitialize() }
    }
}

unsafe fn endpoint(manager: &IMMDeviceEnumerator, id: &str) -> Result<IAudioEndpointVolume> {
    let wide: Vec<u16> = id.encode_utf16().chain(Some(0)).collect();
    let device = manager.GetDevice(PCWSTR(wide.as_ptr()))?;
    if device.GetState()? != DEVICE_STATE_ACTIVE {
        return Err("Selected microphone is disconnected or disabled.".into());
    }
    let capture = manager.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)?;
    let mut found = false;
    for i in 0..capture.GetCount()? {
        if device_id(&capture.Item(i)?)? == id {
            found = true;
            break;
        }
    }
    if !found {
        return Err("Selected endpoint is not an active microphone.".into());
    }
    Ok(device.Activate(CLSCTX_ALL, None)?)
}

unsafe fn device_name(device: &IMMDevice) -> Result<String> {
    let store = device.OpenPropertyStore(STGM_READ)?;
    let mut value = store.GetValue(&PKEY_Device_FriendlyName)?;
    let raw = StructuredStorage::PropVariantToStringAlloc(&value);
    StructuredStorage::PropVariantClear(&mut value)?;
    let raw = raw?;
    let name = raw.to_string();
    CoTaskMemFree(Some(raw.0.cast()));
    Ok(name?)
}

unsafe fn device_id(device: &IMMDevice) -> Result<String> {
    let raw = device.GetId()?;
    let result = raw.to_string();
    CoTaskMemFree(Some(raw.0.cast()));
    Ok(result?)
}

unsafe fn apply(manager: &IMMDeviceEnumerator, id: &str, action: &Action) -> Result<()> {
    let Outcome::MuteState { muted, .. } = perform(manager, id, action)? else {
        return Err("Expected a mute state for this action.".into());
    };
    println!("{}: {}", if muted { "MUTED" } else { "LIVE" }, id);
    Ok(())
}

/// The observable result of a device action.
///
/// `muted` is read back from the platform after the change, never assumed from
/// the request. Reporting the device that was actually touched keeps the caller
/// from describing a different endpoint than the one that changed.
#[derive(Debug, PartialEq, Clone)]
pub(crate) enum Outcome {
    /// The mute state read back after acting on one named endpoint.
    MuteState { device_id: String, muted: bool },
    /// The active capture endpoints, as `(name, id)` pairs.
    Devices(Vec<(String, String)>),
}

/// Runs one device action and returns the state read back from the platform.
///
/// This is the single executor shared by the CLI and the service: both paths
/// must apply the same readback and error contracts. The readback check is the
/// point — a requested mute that the platform did not apply is an error, not a
/// silently assumed success.
pub(crate) unsafe fn perform(
    manager: &IMMDeviceEnumerator,
    id: &str,
    action: &Action,
) -> Result<Outcome> {
    let volume = endpoint(manager, id)?;
    let before = volume.GetMute()?.as_bool();
    let desired = match action {
        Action::Mute => Some(true),
        Action::Unmute => Some(false),
        Action::Toggle => Some(!before),
        _ => None,
    };
    if let Some(muted) = desired {
        volume.SetMute(muted, std::ptr::null())?;
    }
    let actual = volume.GetMute()?.as_bool();
    if desired.is_some_and(|wanted| wanted != actual) {
        return Err("Microphone mute readback did not match requested state.".into());
    }
    Ok(Outcome::MuteState {
        device_id: id.to_string(),
        muted: actual,
    })
}

/// Lists active capture endpoints as `(name, id)` pairs.
pub(crate) unsafe fn capture_devices(
    manager: &IMMDeviceEnumerator,
) -> Result<Vec<(String, String)>> {
    let devices = manager.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)?;
    let mut found = Vec::new();
    for i in 0..devices.GetCount()? {
        let device = devices.Item(i)?;
        found.push((device_name(&device)?, device_id(&device)?));
    }
    Ok(found)
}

/// Executes one action against the platform audio stack.
///
/// Shared by both entry points so the CLI and the service cannot drift. The
/// device id is always explicit: there is no default or fallback endpoint, so
/// an action can never be applied to a microphone the caller did not name.
pub(crate) fn execute(action: Action, id: Option<String>) -> Result<Outcome> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        let _com = Com;
        let manager: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        if action == Action::Devices {
            return Ok(Outcome::Devices(capture_devices(&manager)?));
        }
        let id = id.ok_or("Specify a microphone endpoint with --device <endpoint-id>.")?;
        perform(&manager, &id, &action)
    }
}

struct Hotkey;
impl Drop for Hotkey {
    fn drop(&mut self) {
        unsafe {
            let _ = UnregisterHotKey(None, 1);
        }
    }
}

pub(crate) fn run(action: Action, id: Option<String>) -> Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        let _com = Com;
        let manager: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        if action == Action::Devices {
            for (name, id) in capture_devices(&manager)? {
                println!("{name}\n  {id}");
            }
            return Ok(());
        }
        let id = id.unwrap();
        if action != Action::Listen {
            return apply(&manager, &id, &action);
        }
        apply(&manager, &id, &Action::Status)?;
        RegisterHotKey(
            None,
            1,
            MOD_CONTROL | MOD_ALT | MOD_SHIFT | MOD_NOREPEAT,
            VK_F9.0 as u32,
        )?;
        let _hotkey = Hotkey;
        println!("Listening: Ctrl+Alt+Shift+F9 toggles this microphone. Ctrl+C exits; mute state is preserved.");
        let mut message = MSG::default();
        loop {
            let result = GetMessageW(&mut message, None, 0, 0).0;
            if result == -1 {
                return Err(windows::core::Error::from_win32().into());
            }
            if result == 0 {
                break;
            }
            if message.message == WM_HOTKEY && message.wParam.0 == 1 {
                if let Err(error) = apply(&manager, &id, &Action::Toggle) {
                    eprintln!("ERROR: {error}");
                }
            }
        }
    }
    Ok(())
}
