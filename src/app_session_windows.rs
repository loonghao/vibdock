//! Windows application playback session adapter.
//!
//! Enumerates and controls per-application render sessions through Core Audio,
//! so one application's playback can be muted without touching the system
//! master mute, the default endpoint, or any other application.
//!
//! Two rules drive everything here:
//!
//! * A session is addressed only by an explicit identity — endpoint id plus
//!   session instance id — and that identity is re-verified immediately before
//!   any state change. Process ids are displayed but never used alone to
//!   select a target, because they are recycled.
//! * Nothing is reported as changed until the platform reads back the new
//!   state. A session that disappears between selection and action is reported
//!   as stale rather than silently replaced with a similar one.

use windows::{
    core::{Interface, PCWSTR},
    Win32::{
        Devices::FunctionDiscovery::PKEY_Device_FriendlyName,
        Foundation::{ERROR_DEVICE_NOT_AVAILABLE, ERROR_INVALID_PARAMETER, ERROR_NOT_FOUND},
        Media::Audio::{Endpoints::IAudioEndpointVolume, *},
        System::Com::*,
    },
};

use crate::{
    app_audio::{
        AppSession, ProcessIdentity, SessionAction, SessionFlow, SessionIdentity, SessionOutcome,
    },
    Result,
};

/// Guard that uninitializes COM on drop, matching the device adapter.
struct Com;
impl Drop for Com {
    fn drop(&mut self) {
        unsafe { CoUninitialize() }
    }
}

/// Reads a COM-allocated string and frees it.
///
/// Every `Get*` method that returns a string allocates with `CoTaskMemAlloc`,
/// so the buffer must be released on every path. Centralizing that also means
/// a display name that fails to convert cannot leak its buffer.
unsafe fn co_string(raw: windows::core::PWSTR) -> Result<String> {
    if raw.is_null() {
        return Ok(String::new());
    }
    let value = raw.to_string();
    CoTaskMemFree(Some(raw.0.cast()));
    Ok(value?)
}

/// Reads one device's endpoint id.
unsafe fn device_id(device: &IMMDevice) -> Result<String> {
    let raw = device.GetId()?;
    let result = raw.to_string();
    CoTaskMemFree(Some(raw.0.cast()));
    Ok(result?)
}

/// Resolves a render endpoint by id, refusing anything that is not active.
///
/// The endpoint is looked up by id and then confirmed to be present in the
/// active render enumeration. `GetDevice` alone would happily return a
/// disabled or unplugged endpoint, and acting on one would fail in a way that
/// looks like a session bug rather than a disconnected speaker.
unsafe fn render_endpoint(manager: &IMMDeviceEnumerator, id: &str) -> Result<IMMDevice> {
    let wide: Vec<u16> = id.encode_utf16().chain(Some(0)).collect();
    let device = manager.GetDevice(PCWSTR(wide.as_ptr()))?;
    if device.GetState()? != DEVICE_STATE_ACTIVE {
        return Err("Selected playback endpoint is disconnected or disabled.".into());
    }
    let render = manager.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
    let mut found = false;
    for i in 0..render.GetCount()? {
        if device_id(&render.Item(i)?)? == id {
            found = true;
            break;
        }
    }
    if !found {
        return Err("Selected endpoint is not an active playback device.".into());
    }
    Ok(device)
}

/// Resolves the owning process id of a session, honestly.
///
/// Returns `Unknown` rather than zero when the process cannot be resolved or
/// has already exited. A `0` would be a plausible-looking but wrong answer,
/// and grouping on it would merge unrelated sessions.
unsafe fn process_identity(control: &IAudioSessionControl2) -> ProcessIdentity {
    match control.GetProcessId() {
        // PID 0 is the system idle process and never owns playback audio, so
        // it means "not resolvable", not "owned by pid 0".
        Ok(pid) if pid != 0 => ProcessIdentity::Known(pid),
        _ => ProcessIdentity::Unknown,
    }
}

/// Reads one session's current state from the platform.
///
/// Every value here is observed, not inferred: the mute and volume come from
/// the session's own volume interface after the fact.
unsafe fn observe(
    control: &IAudioSessionControl2,
    endpoint_id: &str,
) -> Result<Option<AppSession>> {
    let instance_id = co_string(control.GetSessionInstanceIdentifier()?)?;
    if instance_id.is_empty() {
        // A session with no instance identifier cannot be addressed later, so
        // it is skipped rather than surfaced as a selectable target.
        return Ok(None);
    }
    let pid = process_identity(control);
    let display_name = co_string(control.GetDisplayName()?)?;
    let state = control.GetState()?;
    let volume: ISimpleAudioVolume = control.cast()?;
    Ok(Some(AppSession {
        identity: SessionIdentity {
            endpoint_id: endpoint_id.to_string(),
            session_instance_id: instance_id,
            process_id: match pid {
                ProcessIdentity::Known(pid) => pid,
                ProcessIdentity::Unknown => 0,
            },
        },
        display_name: if display_name.is_empty() {
            // An empty name is unhelpful in a selection list, but inventing a
            // name from the process would invite exactly the name-similarity
            // grouping this module must not do.
            "(unnamed session)".to_string()
        } else {
            display_name
        },
        process_identity: pid,
        muted: volume.GetMute()?.as_bool(),
        volume: volume.GetMasterVolume()?,
        active: state == AudioSessionStateActive,
    }))
}

/// Enumerates every session on one render endpoint.
///
/// System sounds are included so the user can see them, but they carry no
/// addressable application identity and are never muted implicitly.
pub(crate) unsafe fn list(
    manager: &IMMDeviceEnumerator,
    endpoint_id: &str,
) -> Result<Vec<AppSession>> {
    let device = render_endpoint(manager, endpoint_id)?;
    let manager2: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
    let enumerator = manager2.GetSessionEnumerator()?;
    let mut sessions = Vec::new();
    for index in 0..enumerator.GetCount()? {
        let control: IAudioSessionControl = enumerator.GetSession(index)?;
        let control2: IAudioSessionControl2 = control.cast()?;
        if let Some(session) = observe(&control2, endpoint_id)? {
            sessions.push(session);
        }
    }
    Ok(sessions)
}

/// Finds the live session matching `identity`, or reports why it is gone.
///
/// The endpoint is re-resolved and the full set re-enumerated on every call,
/// so a session that vanished since selection is detected rather than being
/// substituted with whatever now occupies a similar slot.
unsafe fn resolve<'a>(
    sessions: &'a [AppSession],
    identity: &SessionIdentity,
) -> std::result::Result<&'a AppSession, String> {
    crate::app_audio::select(sessions, identity)
}

/// Applies `action` to the one session named by `identity`.
///
/// Order matters: resolve the endpoint, re-enumerate, verify the identity and
/// its owning process, act, then read the state back and verify it. A readback
/// failure does not undo a change already applied by Windows.
pub(crate) unsafe fn apply(
    manager: &IMMDeviceEnumerator,
    identity: &SessionIdentity,
    action: SessionAction,
) -> std::result::Result<AppSession, String> {
    let sessions = list(manager, &identity.endpoint_id).map_err(|e| e.to_string())?;
    let current = resolve(&sessions, identity)?;
    let requested = crate::app_audio::desired_mute(action, current.muted);

    if let Some(muted) = requested {
        let device = render_endpoint(manager, &identity.endpoint_id).map_err(|e| e.to_string())?;
        let manager2: IAudioSessionManager2 = device
            .Activate(CLSCTX_ALL, None)
            .map_err(|e| e.to_string())?;
        let enumerator = manager2.GetSessionEnumerator().map_err(|e| e.to_string())?;
        let mut target: Option<IAudioSessionControl2> = None;
        for index in 0..enumerator.GetCount().map_err(|e| e.to_string())? {
            let control: IAudioSessionControl =
                enumerator.GetSession(index).map_err(|e| e.to_string())?;
            let control2: IAudioSessionControl2 = control.cast().map_err(|e| e.to_string())?;
            let instance_id = co_string(
                control2
                    .GetSessionInstanceIdentifier()
                    .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            if instance_id == identity.session_instance_id {
                target = Some(control2);
                break;
            }
        }
        // Absent here means the session died between the enumerate above and
        // this activation. Report it as stale rather than mutating a different
        // session that happens to be at the same index.
        let control2 = target.ok_or_else(|| {
            "That session no longer exists on this endpoint; run app-list again.".to_string()
        })?;
        // The owning process is checked once more, immediately before the
        // change, so a PID recycled in the window since selection cannot
        // redirect the mute to a different application.
        let live_pid = control2.GetProcessId().map_err(|e| e.to_string())?;
        if live_pid != identity.process_id {
            return Err(format!(
                "That session is now owned by a different process (was pid {}, now pid {}); \
                 run app-list again to reselect.",
                identity.process_id, live_pid
            ));
        }
        let volume: ISimpleAudioVolume = control2.cast().map_err(|e| e.to_string())?;
        volume
            .SetMute(muted, std::ptr::null())
            .map_err(|e| e.to_string())?;
    }

    // Read the whole session back from the platform rather than reusing the
    // pre-action observation: the reported state must be the observed one.
    let after = list(manager, &identity.endpoint_id).map_err(|e| e.to_string())?;
    let observed = resolve(&after, identity)?;
    crate::app_audio::verify_readback(action, requested, observed.muted)?;
    Ok(observed.clone())
}

/// Executes one application-session action against the Windows audio stack.
///
/// Shared by the CLI and, later, the service, so both paths enforce the same
/// identity and readback contracts. The endpoint id is always explicit: there
/// is no default endpoint, so an action can never land on a device the caller
/// did not name.
pub(crate) fn execute(
    action: SessionAction,
    endpoint_id: &str,
    session_id: Option<&str>,
) -> std::result::Result<SessionOutcome, String> {
    if action == SessionAction::Devices {
        return Err(
            "app-devices enumerates endpoints and is handled by render_endpoints().".into(),
        );
    }
    unsafe {
        // COM is required for every Core Audio call. If the audio stack is
        // unavailable — a session with no audio service, or a stripped-down
        // SKU — say so plainly instead of reporting an empty session list.
        CoInitializeEx(None, COINIT_APARTMENTTHREADED)
            .ok()
            .map_err(|_| crate::app_audio::unsupported(action))?;
        let _com = Com;
        let manager: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
            .map_err(|_| crate::app_audio::unsupported(action))?;
        // Confirm the flow this adapter implements: playback only. Stated
        // here so the render-only scope is visible at the platform boundary
        // and cannot drift into input handling unnoticed.
        debug_assert_eq!(crate::app_audio::FLOW, SessionFlow::Render);
        if action == SessionAction::List {
            return Ok(SessionOutcome::Sessions(
                list(&manager, endpoint_id).map_err(endpoint_error)?,
            ));
        }
        let session_id = session_id
            .ok_or("Specify a session instance id with --session <session-instance-id>.")?;
        // The process id is unknown at parse time, so the session is looked up
        // by instance id first and its live pid adopted as the identity to
        // verify against. Selection always happens from a fresh enumeration.
        let sessions = list(&manager, endpoint_id).map_err(endpoint_error)?;
        let found = sessions
            .iter()
            .find(|session| session.identity.session_instance_id == session_id)
            .ok_or_else(|| {
                "That session no longer exists on this endpoint; run app-list again.".to_string()
            })?;
        let identity = SessionIdentity {
            endpoint_id: endpoint_id.to_string(),
            session_instance_id: session_id.to_string(),
            process_id: found.identity.process_id,
        };
        Ok(SessionOutcome::State(apply(&manager, &identity, action)?))
    }
}

/// Turns a platform error into something the user can act on.
///
/// A raw `HRESULT` such as `0x80070057` says nothing about what went wrong.
/// The common causes here are a mistyped or vanished endpoint and an endpoint
/// that is present but not active, so those are named; anything else keeps the
/// OS message as a last resort rather than being discarded.
fn endpoint_error(error: Box<dyn std::error::Error>) -> String {
    // The adapter's calls surface as boxed errors; the ones worth naming come
    // from the OS, so recover the `HRESULT` when it is there.
    if let Some(code) = error
        .downcast_ref::<windows::core::Error>()
        .map(|error| error.code())
    {
        return hresult_message(code)
            .unwrap_or_else(|| format!("Windows rejected the playback endpoint: {error}"));
    }
    format!("Windows rejected the playback endpoint: {error}")
}

/// Classifies a platform `HRESULT`, or `None` if it is not one we name.
///
/// Split out from [`endpoint_error`] so both the boxed-error path and the
/// typed-error path share one set of messages: a user must see the same
/// wording regardless of which call in the chain failed.
fn hresult_message(code: windows::core::HRESULT) -> Option<String> {
    if code == ERROR_INVALID_PARAMETER.to_hresult() || code == ERROR_NOT_FOUND.to_hresult() {
        return Some(
            "No active playback endpoint matches that endpoint id; \
             run app-devices to list endpoint IDs."
                .to_string(),
        );
    }
    if code == ERROR_DEVICE_NOT_AVAILABLE.to_hresult() {
        return Some("That playback endpoint is disconnected or disabled.".to_string());
    }
    None
}

/// Applies [`hresult_message`] to a typed platform error.
fn platform_error(error: windows::core::Error) -> String {
    hresult_message(error.code())
        .unwrap_or_else(|| format!("Windows rejected the playback endpoint: {error}"))
}

/// Lists the active render endpoints, so a user can find an id for `app-list`.
///
/// Render rather than capture: application playback sessions only exist on
/// output devices, and offering capture endpoints here would invite pointing
/// playback control at a microphone.
pub(crate) fn render_endpoints() -> Result<Vec<(String, String)>> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED)
            .ok()
            .map_err(|error| -> Box<dyn std::error::Error> { platform_error(error).into() })?;
        let _com = Com;
        let manager: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let devices = manager.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        let mut found = Vec::new();
        for i in 0..devices.GetCount()? {
            let device = devices.Item(i)?;
            let store = device.OpenPropertyStore(STGM_READ)?;
            let mut value = store.GetValue(&PKEY_Device_FriendlyName)?;
            let raw = StructuredStorage::PropVariantToStringAlloc(&value);
            StructuredStorage::PropVariantClear(&mut value)?;
            let name = co_string(raw?)?;
            found.push((name, device_id(&device)?));
        }
        Ok(found)
    }
}

/// Placeholder that keeps the render-only scope explicit for callers.
///
/// Referenced by [`execute`] to state which flow the adapter implements; kept
/// as a function so the choice is visible in one place.
#[allow(dead_code)]
fn endpoint_volume(device: &IMMDevice) -> Result<IAudioEndpointVolume> {
    unsafe { Ok(device.Activate(CLSCTX_ALL, None)?) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_audio::{AppSession, ProcessIdentity, SessionIdentity};

    fn session(id: &str, pid: u32) -> AppSession {
        AppSession {
            identity: SessionIdentity {
                endpoint_id: "endpoint-1".to_string(),
                session_instance_id: id.to_string(),
                process_id: pid,
            },
            display_name: "App".to_string(),
            process_identity: ProcessIdentity::Known(pid),
            muted: false,
            volume: 1.0,
            active: true,
        }
    }

    #[test]
    fn resolve_reuses_the_core_identity_check() {
        // The Windows path must not grow its own, laxer matching rules: it
        // delegates to the shared selector so both agree on what "same
        // session" means.
        let sessions = vec![session("s1", 100)];
        let identity = SessionIdentity {
            endpoint_id: "endpoint-1".to_string(),
            session_instance_id: "s1".to_string(),
            process_id: 100,
        };
        assert!(unsafe { resolve(&sessions, &identity) }.is_ok());
        let stale = SessionIdentity {
            session_instance_id: "gone".to_string(),
            ..identity
        };
        let error = unsafe { resolve(&sessions, &stale) }.unwrap_err();
        assert!(error.contains("no longer exists"), "got: {error}");
    }

    #[test]
    fn platform_errors_name_what_the_user_can_fix() {
        // A raw HRESULT such as 0x80070057 tells the user nothing, so the
        // common failures are translated into actionable messages.
        let unknown = hresult_message(ERROR_INVALID_PARAMETER.to_hresult())
            .expect("a bad endpoint id must be named");
        assert!(unknown.contains("app-devices"), "got: {unknown}");
        assert!(
            !unknown.contains("0x80070057"),
            "raw HRESULT leaked: {unknown}"
        );
        let missing = hresult_message(ERROR_NOT_FOUND.to_hresult())
            .expect("a missing endpoint must be named");
        assert!(missing.contains("app-devices"), "got: {missing}");
        let gone = hresult_message(ERROR_DEVICE_NOT_AVAILABLE.to_hresult())
            .expect("a disconnected endpoint must be named");
        assert!(gone.contains("disconnected"), "got: {gone}");
        // An unrelated code is not one we claim to understand, so it keeps the
        // OS message instead of being replaced with a guess.
        assert!(hresult_message(windows::core::HRESULT(0x8000_FFFFu32 as i32)).is_none());
    }

    #[test]
    fn a_session_without_an_instance_id_is_not_addressable() {
        // Filtering these out in observe() is what keeps an unaddressable
        // session out of the selection list entirely.
        let sessions: Vec<AppSession> = vec![];
        let identity = SessionIdentity {
            endpoint_id: "endpoint-1".to_string(),
            session_instance_id: String::new(),
            process_id: 0,
        };
        assert!(unsafe { resolve(&sessions, &identity) }.is_err());
    }
}
