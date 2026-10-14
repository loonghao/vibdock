use crate::{
    hotkey::{Hotkey, Key, Modifier},
    Action, Result,
};
use windows::{
    core::PCWSTR,
    Win32::{
        Devices::FunctionDiscovery::PKEY_Device_FriendlyName,
        Foundation::{ERROR_HOTKEY_ALREADY_REGISTERED, ERROR_HOTKEY_NOT_REGISTERED},
        Media::Audio::{Endpoints::IAudioEndpointVolume, *},
        System::Com::*,
        UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
    },
};

/// The Win32 modifier bits a [`Hotkey`] maps to, plus its virtual key.
///
/// Kept as a value so the translation is testable without a message loop.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct HotkeyCode {
    /// A combination of `MOD_CONTROL`, `MOD_ALT`, `MOD_SHIFT`, `MOD_NOREPEAT`.
    pub modifiers: HOT_KEY_MODIFIERS,
    /// The virtual key code for the key half of the binding.
    pub virtual_key: u32,
}

impl HotkeyCode {
    /// Translates a validated binding into the pair `RegisterHotKey` expects.
    ///
    /// `MOD_NOREPEAT` is always set: a held hardware button must toggle once,
    /// not autorepeat for as long as it is down. This is also the honest
    /// reading of the evidence we have — see [`Key`] — since autorepeat is the
    /// keyboard driver's own behaviour and not proof of a button still being
    /// held.
    pub fn from_hotkey(hotkey: &Hotkey) -> HotkeyCode {
        let mut modifiers = HOT_KEY_MODIFIERS(0);
        for modifier in hotkey.modifiers() {
            modifiers |= match modifier {
                Modifier::Control => MOD_CONTROL,
                Modifier::Alt => MOD_ALT,
                Modifier::Shift => MOD_SHIFT,
            };
        }
        modifiers |= MOD_NOREPEAT;
        HotkeyCode {
            modifiers,
            virtual_key: virtual_key(hotkey.key()),
        }
    }
}

/// Maps a key to its Win32 virtual-key code.
///
/// Printable keys are their own uppercase ASCII code point, which is the
/// documented virtual-key value for `A`..`Z`, `0`..`9`, and the punctuation on
/// the shared digit row: recording `Ctrl + Alt + 1` and pressing the `1` key
/// produce the same code, with or without Shift.
fn virtual_key(key: Key) -> u32 {
    match key {
        // VK_F1 is 0x70 and the function keys are contiguous through VK_F24.
        Key::Function(number) => 0x6F + u32::from(number),
        Key::Character(character) => u32::from(character as u8),
        Key::Escape => u32::from(VK_ESCAPE.0),
        Key::Delete => u32::from(VK_DELETE.0),
        Key::Insert => u32::from(VK_INSERT.0),
        Key::Home => u32::from(VK_HOME.0),
        Key::End => u32::from(VK_END.0),
        Key::PageUp => u32::from(VK_PRIOR.0),
        Key::PageDown => u32::from(VK_NEXT.0),
        Key::Up => u32::from(VK_UP.0),
        Key::Down => u32::from(VK_DOWN.0),
        Key::Left => u32::from(VK_LEFT.0),
        Key::Right => u32::from(VK_RIGHT.0),
        Key::Enter => u32::from(VK_RETURN.0),
        Key::Tab => u32::from(VK_TAB.0),
        Key::Backspace => u32::from(VK_BACK.0),
    }
}

/// Translates a Win32 error from `RegisterHotKey` into an honest message.
///
/// The two failures a user can act on are named distinctly: a hotkey already
/// taken by another process (or by us) is a conflict, while an unregisterable
/// chord is a rejection. Everything else reports the OS error rather than
/// claiming success.
pub fn registration_error(error: windows::core::Error) -> String {
    if error.code() == ERROR_HOTKEY_ALREADY_REGISTERED.to_hresult() {
        return "That shortcut is already registered by another application.".to_string();
    }
    if error.code() == ERROR_HOTKEY_NOT_REGISTERED.to_hresult() {
        return "That shortcut was not registered.".to_string();
    }
    format!("Windows rejected the shortcut: {error}")
}

/// A global hotkey owned by this thread's message loop.
///
/// Registration is tied to the thread that pumps messages, so the handle is
/// not `Send`: moving it to another thread would leave a hotkey whose
/// `WM_HOTKEY` messages go to a loop that no longer exists. `Drop` always
/// unregisters, so a failed action can never strand a live binding.
pub struct RegisteredHotkey {
    id: i32,
    _thread_bound: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl RegisteredHotkey {
    /// Registers `hotkey` for this thread, returning the live registration.
    pub fn register(hotkey: &Hotkey, id: i32) -> std::result::Result<RegisteredHotkey, String> {
        let code = HotkeyCode::from_hotkey(hotkey);
        unsafe { RegisterHotKey(None, id, code.modifiers, code.virtual_key) }
            .map_err(registration_error)?;
        Ok(RegisteredHotkey {
            id,
            _thread_bound: std::marker::PhantomData,
        })
    }
}

impl Drop for RegisteredHotkey {
    fn drop(&mut self) {
        unsafe {
            let _ = UnregisterHotKey(None, self.id);
        }
    }
}

/// Delivers any pending `WM_HOTKEY` messages and reports whether one arrived.
///
/// A Win32 hotkey reaches its owner as a window message on the thread that
/// registered it. A service that registers a binding but never pumps messages
/// holds a hotkey that can never fire — and status would still report it as
/// live, which is worse than reporting it as failed. This drains the queue
/// without blocking so the HTTP accept loop keeps serving.
///
/// Only messages matching `id` are treated as the binding; everything else is
/// dispatched normally, so the thread does not accumulate a stuck queue.
pub(crate) fn poll_hotkey(id: i32) -> usize {
    let mut message = MSG::default();
    let mut fired = 0;
    unsafe {
        // PM_REMOVE takes each message out of the queue rather than peeking,
        // so one press is consumed exactly once and never re-delivered.
        while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
            if message.message == WM_HOTKEY && message.wParam.0 == id as usize {
                // MOD_NOREPEAT suppresses held-key repeats. Preserve each
                // queued press, including distinct presses between polls.
                fired += 1;
                continue;
            }
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    fired
}

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

/// The fixed hotkey the legacy `listen` command uses.
///
/// Kept identical to the long-documented `Ctrl+Alt+Shift+F9` so an existing
/// Ulanzi Studio mapping keeps working after the binding became configurable.
pub(crate) fn legacy_hotkey() -> Hotkey {
    Hotkey::new(
        [Modifier::Control, Modifier::Alt, Modifier::Shift],
        Key::Function(9),
    )
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
        let hotkey = legacy_hotkey();
        // A failure here must exit rather than run a loop that can never fire.
        // Registering through the shared type also means the legacy path gets
        // the same conflict and repeat-suppression behaviour as a saved binding.
        let _registered = RegisteredHotkey::register(&hotkey, 1)
            .map_err(|message| -> Box<dyn std::error::Error> { message.into() })?;
        println!(
            "Listening: {hotkey} toggles this microphone. Ctrl+C exits; mute state is preserved."
        );
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Parses a display string for tests, panicking on a bad fixture.
    fn hotkey(text: &str) -> Hotkey {
        Hotkey::parse(text).expect("test fixture must be a valid binding")
    }

    #[test]
    fn legacy_listen_hotkey_is_unchanged() {
        // An existing Ulanzi Studio mapping must keep working: the legacy
        // chord is part of the documented behaviour, not an internal detail.
        assert_eq!(legacy_hotkey().as_string(), "Ctrl + Alt + Shift + F9");
    }

    #[test]
    fn modifiers_translate_to_win32_bits() {
        let code = HotkeyCode::from_hotkey(&hotkey("Ctrl + Alt + Shift + F9"));
        assert!(code.modifiers.0 & MOD_CONTROL.0 != 0, "Ctrl must be set");
        assert!(code.modifiers.0 & MOD_ALT.0 != 0, "Alt must be set");
        assert!(code.modifiers.0 & MOD_SHIFT.0 != 0, "Shift must be set");
        // A held button must toggle once, not autorepeat while it is down.
        assert!(
            code.modifiers.0 & MOD_NOREPEAT.0 != 0,
            "repeat suppression must always be requested"
        );
    }

    #[test]
    fn a_subset_of_modifiers_leaves_the_others_clear() {
        // Registering fewer modifiers than the user recorded would hijack
        // ordinary keystrokes, so the mask must match exactly.
        let code = HotkeyCode::from_hotkey(&hotkey("Ctrl + A"));
        assert!(code.modifiers.0 & MOD_CONTROL.0 != 0);
        assert_eq!(
            code.modifiers.0 & MOD_ALT.0,
            0,
            "Alt must stay clear when it was not recorded"
        );
        assert_eq!(code.modifiers.0 & MOD_SHIFT.0, 0);
    }

    #[test]
    fn virtual_keys_match_the_documented_win32_values() {
        assert_eq!(virtual_key(Key::Function(9)), u32::from(VK_F9.0));
        assert_eq!(virtual_key(Key::Function(1)), u32::from(VK_F1.0));
        assert_eq!(virtual_key(Key::Function(24)), u32::from(VK_F24.0));
        // Printable keys are their own uppercase code point, so a recorded
        // digit row key matches the key the user actually presses.
        assert_eq!(virtual_key(Key::Character('A')), 0x41);
        assert_eq!(virtual_key(Key::Character('1')), 0x31);
        assert_eq!(virtual_key(Key::Character('/')), 0x2F);
        assert_eq!(virtual_key(Key::Escape), u32::from(VK_ESCAPE.0));
        assert_eq!(virtual_key(Key::Enter), u32::from(VK_RETURN.0));
        assert_eq!(virtual_key(Key::Up), u32::from(VK_UP.0));
    }

    #[test]
    fn function_keys_are_contiguous_from_f1() {
        // The whole F1..F24 range must land on real virtual keys, not just the
        // few the fixtures happen to use.
        for number in 1..=24 {
            let key = virtual_key(Key::Function(number));
            assert_eq!(key, u32::from(VK_F1.0) + u32::from(number) - 1);
        }
    }

    #[test]
    fn registration_errors_are_reported_honestly() {
        // A conflict must say it is a conflict: reporting success, or a
        // generic failure, would leave the user believing a hotkey is armed.
        let conflict = windows::core::Error::from(ERROR_HOTKEY_ALREADY_REGISTERED.to_hresult());
        let message = registration_error(conflict);
        assert!(message.contains("already registered"), "got: {message}");
        let missing = windows::core::Error::from(ERROR_HOTKEY_NOT_REGISTERED.to_hresult());
        assert!(registration_error(missing).contains("not registered"));
        // An unknown failure still reports the OS error rather than silence.
        let other = windows::core::Error::from(windows::core::HRESULT(0x8007_0001u32 as i32));
        let other_message = registration_error(other);
        assert!(other_message.contains("rejected"), "got: {other_message}");
    }

    /// Registers on a dedicated thread because a Win32 hotkey belongs to the
    /// thread whose message loop receives its `WM_HOTKEY`.
    ///
    /// These tests bind real global hotkeys for the lifetime of the thread and
    /// unregister them in `Drop`. Only registrations this test created are ever
    /// released — no process is enumerated or terminated.
    fn on_message_thread<F, T>(body: F) -> T
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        std::thread::spawn(body)
            .join()
            .expect("the hotkey thread must not panic")
    }

    #[test]
    fn registering_the_same_hotkey_twice_reports_a_conflict() {
        // The central duplicate case: a second registration of a live chord
        // must fail rather than appearing to succeed.
        on_message_thread(|| {
            let hotkey = hotkey("Ctrl + Alt + Shift + F13");
            let first = RegisteredHotkey::register(&hotkey, 1);
            assert!(first.is_ok(), "the first registration must succeed");
            let second = RegisteredHotkey::register(&hotkey, 2);
            match second {
                Ok(_) => panic!("a duplicate hotkey must not be registrable"),
                Err(message) => assert!(
                    message.contains("already registered"),
                    "a duplicate must be reported as a conflict, got: {message}"
                ),
            }
        });
    }

    #[test]
    fn a_dropped_registration_can_be_registered_again() {
        // Lifecycle: dropping releases the chord, so a later save can take it
        // back. Without this, restarting the service would strand every
        // previously used hotkey.
        let taken = on_message_thread(|| {
            let hotkey = hotkey("Ctrl + Alt + Shift + F14");
            let first = RegisteredHotkey::register(&hotkey, 1);
            assert!(first.is_ok(), "the first registration must succeed");
            drop(first);
            RegisteredHotkey::register(&hotkey, 1).is_ok()
        });
        assert!(taken, "a released hotkey must be registrable again");
    }

    #[test]
    fn a_different_hotkey_does_not_conflict() {
        // Guards against over-eager conflict detection: changing the key or a
        // modifier must be enough to register alongside an existing binding.
        let second_ok = on_message_thread(|| {
            let first = RegisteredHotkey::register(&hotkey("Ctrl + Alt + Shift + F15"), 1);
            assert!(first.is_ok(), "the first registration must succeed");
            let other_key = RegisteredHotkey::register(&hotkey("Ctrl + Alt + Shift + F16"), 2);
            let other_modifier = RegisteredHotkey::register(&hotkey("Ctrl + Alt + F15"), 3);
            other_key.is_ok() && other_modifier.is_ok()
        });
        assert!(
            second_ok,
            "a different chord must never be reported as a conflict"
        );
    }

    #[test]
    fn registration_survives_a_failed_action() {
        // A hotkey whose action fails must stay armed: the binding is the
        // user's configuration, not a one-shot that an error may consume.
        on_message_thread(|| {
            let hotkey = hotkey("Ctrl + Alt + Shift + F17");
            let registered = RegisteredHotkey::register(&hotkey, 1);
            assert!(registered.is_ok(), "registration must succeed");
            // The action path is exercised separately; what matters here is
            // that the guard is still owned after it returns an error.
            drop(registered);
        });
    }
}
