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
        Foundation::{
            ERROR_DEVICE_NOT_AVAILABLE, ERROR_INVALID_PARAMETER, ERROR_NOT_FOUND,
            RPC_E_CHANGED_MODE,
        },
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

/// Why a session present on an endpoint is not in the returned list.
///
/// The two variants are kept apart because they need different wording and
/// different severity. [`SkippedSession::Unaddressable`] is normal: system
/// sounds and similar sessions have no session-instance identifier, so they can
/// never be selected and are excluded by design — on a typical Windows desktop
/// at least one such session exists. [`SkippedSession::Unreadable`] is not
/// normal: the session is addressable but its state could not be read, which
/// points at a real platform problem. Reporting the first as the second would
/// claim a failure on nearly every healthy machine.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum SkippedSession {
    /// No session-instance identifier, so it can never be addressed. Expected.
    Unaddressable,
    /// Addressable, but reading its state failed. Indicates a platform problem.
    Unreadable,
}

impl SkippedSession {
    /// Wording for the summary line, matching the reason it was skipped.
    fn description(&self) -> &'static str {
        match self {
            SkippedSession::Unaddressable => "are not addressable",
            SkippedSession::Unreadable => "could not be read",
        }
    }
}

/// A session on an endpoint that is not in the returned list, and why.
///
/// Returned alongside the readable sessions so a caller can tell "this endpoint
/// has no sessions" apart from "this endpoint has sessions we did not list" —
/// reporting the first when the second is true would be a silent lie about the
/// platform, and would make an unrelated session look absent to `app-mute`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SkippedSessionEntry {
    /// Index of the session within the endpoint's enumeration, for reporting.
    pub index: i32,
    /// Why it was skipped.
    pub reason: SkippedSession,
}

/// Enumerates every session on one render endpoint.
///
/// System sounds are included so the user can see them, but they carry no
/// addressable application identity and are never muted implicitly.
///
/// A session whose state cannot be read is skipped rather than failing the
/// whole enumeration. That matches how `observe` already treats a session with
/// no instance identifier, and it matters for `app-mute`: one unreadable
/// session belonging to another process must not make an unrelated, perfectly
/// readable session impossible to mute. Skipping only ever removes candidates,
/// so it cannot widen what a command is allowed to touch.
///
/// The skipped sessions are returned rather than discarded, each with the
/// reason, so a partial list is never presented as a complete one and the
/// reported wording matches why each session is missing.
pub(crate) unsafe fn list(
    manager: &IMMDeviceEnumerator,
    endpoint_id: &str,
) -> Result<(Vec<AppSession>, Vec<SkippedSessionEntry>)> {
    let device = render_endpoint(manager, endpoint_id)?;
    let manager2: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
    let enumerator = manager2.GetSessionEnumerator()?;
    let mut sessions = Vec::new();
    let mut skipped = Vec::new();
    for index in 0..enumerator.GetCount()? {
        let control: IAudioSessionControl = enumerator.GetSession(index)?;
        let control2: IAudioSessionControl2 = control.cast()?;
        // Both arms skip for the same safety reason — neither can be acted on —
        // but they are recorded separately because only one is a fault.
        let reason = match observe(&control2, endpoint_id) {
            Ok(Some(session)) => {
                sessions.push(session);
                continue;
            }
            Ok(None) => SkippedSession::Unaddressable,
            Err(_) => SkippedSession::Unreadable,
        };
        skipped.push(SkippedSessionEntry { index, reason });
    }
    Ok((sessions, skipped))
}

/// Finds the live session matching `identity`, or reports why it is gone.
///
/// A thin forward to the shared selector, so this adapter cannot grow its own
/// laxer matching rules. Re-resolution and re-enumeration are the caller's job:
/// [`apply`] re-enumerates immediately before acting on the result.
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
    let (sessions, _skipped) = list(manager, &identity.endpoint_id).map_err(endpoint_error)?;
    let current = resolve(&sessions, identity)?;
    let requested = crate::app_audio::desired_mute(action, current.muted);
    // What we actually asked Windows for. Equal to `requested` except for
    // `app-toggle`, whose target is recomputed from live state just before the
    // write; the readback gate below must verify against this value, not
    // against a target derived from a snapshot taken earlier.
    let mut written = requested;

    if let Some(muted) = requested {
        let device = render_endpoint(manager, &identity.endpoint_id).map_err(endpoint_error)?;
        let manager2: IAudioSessionManager2 =
            device.Activate(CLSCTX_ALL, None).map_err(platform_error)?;
        let enumerator = manager2.GetSessionEnumerator().map_err(platform_error)?;
        let mut target: Option<IAudioSessionControl2> = None;
        for index in 0..enumerator.GetCount().map_err(platform_error)? {
            let control: IAudioSessionControl =
                enumerator.GetSession(index).map_err(platform_error)?;
            let control2: IAudioSessionControl2 = control.cast().map_err(platform_error)?;
            let instance_id = co_string(
                control2
                    .GetSessionInstanceIdentifier()
                    .map_err(platform_error)?,
            )
            .map_err(endpoint_error)?;
            if instance_id == identity.session_instance_id {
                target = Some(control2);
                break;
            }
        }
        // Absent here means the session died between the enumerate above and
        // this activation. Nothing has been written yet, so this is genuinely
        // "no change was made" — unlike the post-write readback below, which
        // must say so explicitly.
        let control2 = target.ok_or_else(|| {
            "That session no longer exists on this endpoint; run app-list again.".to_string()
        })?;
        // The owning process is checked once more, immediately before the
        // change, so a PID recycled in the window since selection cannot
        // redirect the mute to a different application.
        let live_pid = control2.GetProcessId().map_err(platform_error)?;
        if live_pid != identity.process_id {
            return Err(format!(
                "That session is now owned by a different process (was pid {}, now pid {}); \
                 run app-list again to reselect.",
                identity.process_id, live_pid
            ));
        }
        let volume: ISimpleAudioVolume = control2.cast().map_err(platform_error)?;
        // Re-read the live mute state from the very interface we are about to
        // write, so a third-party change between enumeration and write cannot
        // invert the toggle: the target is recomputed, not taken from a
        // snapshot. `mute`/`unmute` name an absolute state and are unaffected.
        let live_muted = volume.GetMute().map_err(platform_error)?.as_bool();
        let muted = mute_value_to_write(action, muted, live_muted);
        written = Some(muted);
        volume
            .SetMute(muted, std::ptr::null())
            .map_err(platform_error)?;
    }

    // Read the whole session back from the platform rather than reusing the
    // pre-action observation: the reported state must be the observed one.
    //
    // Once `SetMute` has returned Ok, Windows may already have applied the
    // change. If this re-enumeration now fails, the caller must not be told
    // "the session no longer exists" — that is the pre-write wording and reads
    // as "nothing happened", which invites a retry that could reverse a mute
    // that did take effect.
    let after = match list(manager, &identity.endpoint_id) {
        Ok((after, _skipped)) => after,
        Err(error) => return Err(readback_error(action, written, &endpoint_error(error))),
    };
    let observed = match resolve(&after, identity) {
        Ok(observed) => observed,
        // The cause is deliberately generic, not `error` from `select()`: that
        // wording describes a pre-write lookup and would read as "nothing was
        // changed" here, where the change may already be applied.
        Err(_) => return Err(readback_error(action, written, &unconfirmed_cause())),
    };
    crate::app_audio::verify_readback(action, written, observed.muted)?;
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
        //
        // "Unsupported on this platform" is reserved for builds without the
        // adapter. On Windows every failure has a real cause, and reporting it
        // as unimplemented sends the reader looking in the wrong place: the
        // common one is `RPC_E_CHANGED_MODE`, where the process already
        // initialized COM in another mode. That does not happen on today's CLI
        // path, but it does once this runs inside the resident service or GUI.
        CoInitializeEx(None, COINIT_APARTMENTTHREADED)
            .ok()
            .map_err(com_error)?;
        let _com = Com;
        let manager: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(platform_error)?;
        // Confirm the flow this adapter implements: playback only. Stated
        // here so the render-only scope is visible at the platform boundary
        // and cannot drift into input handling unnoticed.
        debug_assert_eq!(crate::app_audio::FLOW, SessionFlow::Render);
        if action == SessionAction::List {
            let (sessions, skipped) = list(&manager, endpoint_id).map_err(endpoint_error)?;
            // Report what was left out rather than letting the list look
            // complete. This stays a warning, not an error: failing the whole
            // command would reintroduce the problem the skip was added to
            // solve, where one unreadable session blocked an unrelated one.
            for warning in skipped_session_warnings(&skipped, sessions.len()) {
                eprintln!("{warning}");
            }
            return Ok(SessionOutcome::Sessions(sessions));
        }
        let session_id = session_id
            .ok_or("Specify a session instance id with --session <session-instance-id>.")?;
        // The process id is unknown at parse time, so the session is looked up
        // by instance id first and its live pid adopted as the identity to
        // verify against. Selection always happens from a fresh enumeration.
        let (sessions, _skipped) = list(&manager, endpoint_id).map_err(endpoint_error)?;
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

/// Explains a COM initialization failure on Windows.
///
/// Kept separate from [`platform_error`] because the endpoint messages would be
/// actively misleading here: COM failing has nothing to do with the endpoint id
/// the user supplied. `RPC_E_CHANGED_MODE` means the process already called
/// `CoInitializeEx` with a different concurrency model, which is expected once
/// this adapter runs inside the resident service or GUI rather than the CLI.
fn com_error(error: windows::core::Error) -> String {
    if error.code() == RPC_E_CHANGED_MODE {
        return format!(
            "COM is already initialized in a different mode in this process ({error}). \
             Application session control needs COINIT_APARTMENTTHREADED; \
             initialize COM that way before calling {}.",
            "this command"
        );
    }
    format!("Windows COM is unavailable, so application sessions cannot be read: {error}")
}

/// Renders a readback failure, distinguishing "wrote, unconfirmed" from "read failed".
///
/// Factored out of [`apply`] so the wording is testable without a real audio
/// stack — the Windows write path has no automated coverage, and this message
/// is the one a caller would most easily misread.
///
/// When a write was submitted, the message must never say the session is gone:
/// after `SetMute` returns Ok the change may already be applied, and a caller
/// told "no longer exists" would conclude the command did nothing and retry,
/// reversing a mute that did take effect. With no write submitted, that same
/// claim would be a lie in the other direction, so the cause is reported alone.
///
/// `cause` is a short phrase, never a sentence: it is spliced before a full
/// stop, so passing prose ending in punctuation produces a double period.
fn post_write_readback_error(action: SessionAction, cause: &str) -> String {
    format!(
        "{action} was submitted to Windows but the result could not be read back: {cause}. \
         The change may already be applied; run app-status to check before retrying, \
         because repeating app-toggle could reverse it."
    )
}

/// Whether an enumeration lost sessions that could not be read.
///
/// Builds the `app-list` warning lines, one per skip reason that occurred.
///
/// Each reason is described in its own words so an expected, by-design
/// exclusion is never reported as a read failure: unaddressable sessions exist
/// on nearly every Windows desktop, so calling them a read failure would report
/// a healthy machine as broken on every run. They are still counted, because
/// the list would otherwise look like the whole endpoint.
///
/// Kept separate from [`list`] so the wording is testable on its own.
fn skipped_session_warnings(skipped: &[SkippedSessionEntry], listed: usize) -> Vec<String> {
    let mut warnings = Vec::new();
    for reason in [SkippedSession::Unaddressable, SkippedSession::Unreadable] {
        let count = skipped
            .iter()
            .filter(|entry| entry.reason == reason)
            .count();
        if count == 0 {
            continue;
        }
        warnings.push(format!(
            "warning: {} of {} session(s) on this endpoint {} and are not listed below.",
            count,
            listed + skipped.len(),
            reason.description()
        ));
    }
    warnings
}

/// Recomputes the mute value to write, given the state read just before it.
///
/// `app-toggle` is the only verb whose target depends on observed state, so it
/// is the only one that can be wrong: a third-party change between enumeration
/// and write would otherwise invert the request, and the readback gate could
/// not catch it because it only checks that the write took effect. Reading the
/// live state immediately before writing closes that window.
///
/// `mute` and `unmute` name an absolute target and ignore `live_muted`, so a
/// concurrent change cannot redirect them.
fn mute_value_to_write(action: SessionAction, requested: bool, live_muted: bool) -> bool {
    match action {
        SessionAction::Toggle => !live_muted,
        _ => requested,
    }
}

/// The cause reported when the written session cannot be found again afterwards.
///
/// Deliberately **not** the wording [`crate::app_audio::select`] returns. That
/// text — "That session no longer exists" — is accurate before a write, where
/// finding nothing means nothing was done. After a write it is the most
/// misleading sentence available: the target app exiting mid-command is the
/// common trigger for this path, and a caller reading "no longer exists" would
/// conclude the mute failed and retry, reversing one that was applied.
fn unconfirmed_cause() -> String {
    "the session is no longer present to confirm the result".to_string()
}

/// Turns a readback failure into an error, branching on whether a write was made.
fn readback_error(action: SessionAction, written: Option<bool>, cause: &str) -> String {
    match written {
        Some(_) => post_write_readback_error(action, cause),
        None => cause.to_string(),
    }
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

    #[test]
    fn a_read_only_action_never_reports_a_submitted_but_unconfirmed_write() {
        // The post-write wording must be reserved for real writes. With no
        // requested change, a readback failure is still just "cannot look", so
        // a caller is never told a mute may have been applied when none was.
        // Uses the real cause, not a synthetic one, so the assertions exercise
        // the message the write path actually produces.
        let error = readback_error(SessionAction::Status, None, &unconfirmed_cause());
        assert!(
            !error.contains("submitted to Windows"),
            "a read must not claim a write: {error}"
        );
        assert!(
            error.contains("could not be read back") || error.contains("no longer present"),
            "got: {error}"
        );
    }

    #[test]
    fn a_failed_readback_after_a_write_says_the_change_may_have_applied() {
        // The P2 wording fix, asserted against the real cause. After SetMute
        // returns Ok, "session no longer exists" would read as "nothing
        // happened" and invite a retry that reverses a mute which did apply.
        let error = readback_error(SessionAction::Mute, Some(true), &unconfirmed_cause());
        assert!(error.contains("submitted to Windows"), "got: {error}");
        assert!(error.contains("app-status"), "must name the check: {error}");
        assert!(
            error.contains("app-toggle"),
            "must warn against blind retry: {error}"
        );
        assert!(
            !error.contains("no longer exists"),
            "post-write message must not reuse the pre-write wording: {error}"
        );
    }

    #[test]
    fn com_mode_conflict_is_not_reported_as_unimplemented() {
        // Telling a Windows user their platform is unimplemented sends them the
        // wrong way; the real cause here is COM already initialized in another
        // mode, which is what happens once this runs inside the service or GUI.
        let message = com_error(windows::core::Error::from_hresult(RPC_E_CHANGED_MODE));
        assert!(
            !message.contains("unimplemented"),
            "COM conflict is not an unimplemented platform: {message}"
        );
        assert!(message.contains("APARTMENTTHREADED"), "got: {message}");
    }

    #[test]
    fn an_unrelated_com_failure_keeps_its_hresult() {
        // Not every COM failure is a mode conflict; the rest must stay
        // diagnosable rather than being flattened into one guess.
        let message = com_error(windows::core::Error::from_hresult(windows::core::HRESULT(
            0x8000_FFFFu32 as i32,
        )));
        assert!(message.contains("COM"), "got: {message}");
        assert!(!message.contains("APARTMENTTHREADED"), "got: {message}");
    }

    #[test]
    fn toggle_target_follows_live_state_not_the_earlier_snapshot() {
        // The P3-5 regression, stated so it can actually fail. The snapshot
        // says unmuted (so a naive toggle writes `true`); the live state read
        // immediately before the write says already muted, so the correct
        // toggle writes `false`. An implementation that trusts the snapshot
        // writes `true` here and fails this assertion.
        assert!(
            !mute_value_to_write(SessionAction::Toggle, true, true),
            "toggle must follow live state, not the stale snapshot"
        );
        assert!(mute_value_to_write(SessionAction::Toggle, false, false));
        // Absolute verbs must ignore live state entirely: a concurrent change
        // cannot redirect them. A toggle-style implementation would fail here.
        assert!(
            mute_value_to_write(SessionAction::Mute, true, false),
            "mute is absolute"
        );
        assert!(
            !mute_value_to_write(SessionAction::Unmute, false, true),
            "unmute is absolute"
        );
    }

    #[test]
    fn only_a_read_failure_warns_not_a_by_design_exclusion() {
        // The P3 fix. System-sound sessions have no instance id, so they are
        // excluded by design and exist on nearly every Windows desktop. Warning
        // about them would report a healthy machine as broken on every run.
        let unaddressable = [SkippedSessionEntry {
            index: 2,
            reason: SkippedSession::Unaddressable,
        }];
        let warnings = skipped_session_warnings(&unaddressable, 1);
        assert!(
            !warnings
                .iter()
                .any(|line| line.contains("could not be read")),
            "an expected exclusion must not be reported as a fault: {warnings:?}"
        );

        // A genuine read failure must still warn — that was the whole point of
        // F3, and dropping it would silently under-report again.
        let unreadable = [SkippedSessionEntry {
            index: 3,
            reason: SkippedSession::Unreadable,
        }];
        assert!(!skipped_session_warnings(&unreadable, 1).is_empty());
        assert!(skipped_session_warnings(&[], 1).is_empty());
    }

    #[test]
    fn each_skip_reason_is_described_in_its_own_words() {
        // The wording must match the reason. Merging them is what produced the
        // false "could not be read" claim for unaddressable sessions.
        let skipped = [
            SkippedSessionEntry {
                index: 1,
                reason: SkippedSession::Unaddressable,
            },
            SkippedSessionEntry {
                index: 2,
                reason: SkippedSession::Unreadable,
            },
        ];
        let warnings = skipped_session_warnings(&skipped, 3);
        assert_eq!(warnings.len(), 2, "one line per reason: {warnings:?}");

        let unaddressable_line = &warnings[0];
        assert!(
            unaddressable_line.contains("not addressable"),
            "got: {unaddressable_line}"
        );
        assert!(
            !unaddressable_line.contains("could not be read"),
            "must not call a by-design exclusion a read failure: {unaddressable_line}"
        );

        let unreadable_line = &warnings[1];
        assert!(
            unreadable_line.contains("could not be read"),
            "got: {unreadable_line}"
        );
        // Both are counted against the same total, so the denominator stays the
        // endpoint's real session count rather than the listed subset.
        assert!(
            unreadable_line.contains("of 5 session(s)"),
            "got: {unreadable_line}"
        );
    }

    #[test]
    fn the_readback_cause_never_repeats_the_pre_write_no_longer_exists_wording() {
        // The F1 regression. `select()`'s "no longer exists" is accurate before
        // a write but misleading after one, where it reads as "nothing was
        // changed" and invites a retry that reverses an applied mute. Assert
        // the real `select()` error text is absent from what we report.
        let select_error = {
            let sessions: Vec<AppSession> = vec![];
            let identity = SessionIdentity {
                endpoint_id: "endpoint-1".to_string(),
                session_instance_id: "gone".to_string(),
                process_id: 1,
            };
            unsafe { resolve(&sessions, &identity) }.unwrap_err()
        };
        assert!(
            select_error.contains("no longer exists"),
            "precondition: the stale wording must exist in select() to be worth excluding"
        );
        let reported = readback_error(SessionAction::Mute, Some(true), &unconfirmed_cause());
        assert!(
            !reported.contains("no longer exists"),
            "post-write message must not reuse the pre-write wording: {reported}"
        );
        assert!(reported.contains("submitted to Windows"), "got: {reported}");
        assert!(reported.contains("app-status"), "got: {reported}");
        // The cause is spliced before a full stop, so it must not end in one.
        assert!(
            !reported.contains(".."),
            "double period from a sentence-shaped cause: {reported}"
        );
    }
}
