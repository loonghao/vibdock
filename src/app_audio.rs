//! Application audio session contract and command surface.
//!
//! This module is the platform-independent half of application playback
//! control: it defines how a session is identified, what the CLI accepts, and
//! how results are rendered. The Windows-only work of talking to Core Audio
//! lives in the platform adapter, which produces the types defined here.
//!
//! The separation matters for the meeting-isolation scenario. Muting one
//! application's playback must never touch the system master mute, the default
//! endpoint, or another application's session. That guarantee is expressed
//! here by making identity explicit and by refusing to act on anything that
//! cannot be re-verified.

use std::fmt;

use crate::Result;

/// Which audio direction a session control acts on.
///
/// Application playback control is only defined for render (output) sessions.
/// Input/device mute is a different operation with different blast radius, and
/// keeping the two apart in the type system is what stops an "app mute" from
/// silently becoming a microphone mute.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum SessionFlow {
    /// Playback sessions: what an application renders to a speaker.
    Render,
}

/// The single flow the application-session commands act on.
///
/// Shared by every platform adapter so the render-only scope of session
/// control is stated once. A platform cannot quietly start handling input
/// sessions without changing this, which is where a reviewer would look.
pub(crate) const FLOW: SessionFlow = SessionFlow::Render;

/// Identifies exactly one application playback session.
///
/// Every field is part of the identity. A session is addressed by its
/// session-instance identifier, which is stable for the lifetime of the
/// session; the endpoint id is required because session ids are only unique
/// within one endpoint. The process id is carried for display and grouping but
/// is deliberately **not** sufficient to act on, because process ids are
/// recycled: a PID alone can name a different process than the one the user
/// picked.
#[derive(Debug, PartialEq, Eq, Clone)]
pub(crate) struct SessionIdentity {
    /// The render endpoint this session belongs to.
    pub endpoint_id: String,
    /// The session instance identifier, unique within the endpoint.
    pub session_instance_id: String,
    /// The owning process id, for display and grouping only.
    pub process_id: u32,
}

/// A session as observed at one instant, with its identity and actual state.
#[derive(Debug, PartialEq, Clone)]
pub(crate) struct AppSession {
    /// How this session is addressed.
    pub identity: SessionIdentity,
    /// Best available human-readable name; not part of the identity.
    pub display_name: String,
    /// Whether the owning process could be resolved.
    ///
    /// `Unknown` is honest here: it means we could not verify who owns the
    /// session, so grouping by name similarity must not be attempted.
    pub process_identity: ProcessIdentity,
    /// Mute state read back from the platform, never assumed from a request.
    pub muted: bool,
    /// Master volume of the session, 0.0 to 1.0.
    pub volume: f32,
    /// Whether the session is currently playing audio.
    pub active: bool,
}

/// How confidently a session's owning process is known.
///
/// This exists so that multi-process grouping cannot guess. Applications that
/// split audio across helper processes (browsers, meeting clients) must be
/// grouped only when identity is verifiable; otherwise the user is asked to
/// choose rather than being given a silently merged target.
#[derive(Debug, PartialEq, Eq, Clone)]
// A resolved owning process is only produced by the Windows adapter, which can
// query the session's process. Elsewhere nothing can construct `Known`, so the
// lint is scoped to the platforms that genuinely lack an adapter rather than
// allowing it everywhere and hiding a real Windows regression.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) enum ProcessIdentity {
    /// The owning process was resolved; carries its id for display.
    Known(u32),
    /// The owning process could not be resolved. Not groupable by guesswork.
    Unknown,
}

impl fmt::Display for ProcessIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProcessIdentity::Known(pid) => write!(f, "pid {pid}"),
            ProcessIdentity::Unknown => f.write_str("unknown process"),
        }
    }
}

/// An action the CLI can perform on one application playback session.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum SessionAction {
    /// Enumerate the render endpoints available for session control.
    Devices,
    /// Enumerate the sessions on an endpoint.
    List,
    /// Read one session's current state.
    Status,
    /// Mute one session.
    Mute,
    /// Unmute one session.
    Unmute,
    /// Flip one session's mute state.
    Toggle,
}

impl fmt::Display for SessionAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            SessionAction::Devices => "app-devices",
            SessionAction::List => "app-list",
            SessionAction::Status => "app-status",
            SessionAction::Mute => "app-mute",
            SessionAction::Unmute => "app-unmute",
            SessionAction::Toggle => "app-toggle",
        };
        f.write_str(name)
    }
}

/// Usage text for the application-session commands.
pub(crate) const SESSION_USAGE: &str = "Usage: vibdock app-devices
       vibdock app-list --endpoint <endpoint-id>
       vibdock <app-status|app-mute|app-unmute|app-toggle> --endpoint <endpoint-id> \\
               --session <session-instance-id>";

/// Parses application-session arguments into an action and its selectors.
///
/// Both selectors are required for every action except `app-list`. A missing
/// session id is an error rather than a default: acting on "whichever session
/// happens to be first" would let a mute land on an application the user never
/// chose.
pub(crate) fn parse_session(
    args: &[String],
) -> Result<(SessionAction, Option<String>, Option<String>)> {
    let action = match args.first().map(String::as_str) {
        Some("app-devices") => SessionAction::Devices,
        Some("app-list") => SessionAction::List,
        Some("app-status") => SessionAction::Status,
        Some("app-mute") => SessionAction::Mute,
        Some("app-unmute") => SessionAction::Unmute,
        Some("app-toggle") => SessionAction::Toggle,
        _ => return Err(SESSION_USAGE.to_string().into()),
    };
    if action == SessionAction::Devices {
        if !args[1..].is_empty() {
            return Err("app-devices does not accept additional arguments.".into());
        }
        return Ok((action, None, None));
    }
    let (endpoint, session) = selectors(&args[1..])?;
    if action == SessionAction::List {
        if session.is_some() {
            return Err("app-list enumerates an endpoint; it does not take --session.".into());
        }
        let endpoint = endpoint.ok_or(
            "Specify a render endpoint with --endpoint <endpoint-id>; run app-list to enumerate.",
        )?;
        return Ok((action, Some(endpoint), None));
    }
    let endpoint = endpoint.ok_or("Specify a render endpoint with --endpoint <endpoint-id>.")?;
    let session = session.ok_or(
        "Specify a session instance id with --session <session-instance-id>; run app-list to enumerate.",
    )?;
    Ok((action, Some(endpoint), Some(session)))
}

/// Reads `--endpoint` and `--session` from an argument list.
///
/// Flags may appear in either order. A flag repeated with different values is
/// rejected so a caller cannot smuggle a second target past the first check.
fn selectors(args: &[String]) -> Result<(Option<String>, Option<String>)> {
    let mut endpoint: Option<String> = None;
    let mut session: Option<String> = None;
    let mut index = 0usize;
    while index < args.len() {
        let flag = args[index].as_str();
        let slot = match flag {
            "--endpoint" => &mut endpoint,
            "--session" => &mut session,
            other => return Err(format!("Unexpected argument `{other}`.\n{SESSION_USAGE}").into()),
        };
        let value = args
            .get(index + 1)
            .ok_or(format!("{flag} requires a value."))?;
        if value.is_empty() || value.contains('\0') || value.len() > 512 {
            return Err(format!("{flag} requires a non-empty identifier.").into());
        }
        match slot {
            Some(existing) if *existing != *value => {
                return Err(
                    format!("{flag} was given more than once with different values.").into(),
                )
            }
            _ => *slot = Some(value.clone()),
        }
        index += 2;
    }
    Ok((endpoint, session))
}

/// The result of a session action, for rendering.
///
/// Carries the state read back from the platform after the change. The read
/// back value is the whole point: a requested mute the platform did not apply
/// is reported as an error upstream, so this can never describe a state that
/// was merely requested.
// `State` is only produced after a session action succeeds, which requires the
// Windows adapter; on other platforms every action fails before reaching it.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, PartialEq, Clone)]
pub(crate) enum SessionOutcome {
    /// The sessions currently present on one endpoint.
    Sessions(Vec<AppSession>),
    /// The state of one session after acting on it.
    State(AppSession),
}

/// Renders an outcome as the machine-readable JSON contract.
///
/// The JSON shape is part of the CLI contract, so it is built here in the
/// cross-platform core and covered by tests that run everywhere. Escaping is
/// done for every string field so a display name containing quotes or
/// backslashes cannot alter the document's structure.
pub(crate) fn to_json(outcome: &SessionOutcome) -> String {
    match outcome {
        SessionOutcome::Sessions(sessions) => {
            let body: Vec<String> = sessions.iter().map(session_json).collect();
            format!("{{\"sessions\":[{}]}}", body.join(","))
        }
        SessionOutcome::State(session) => session_json(session),
    }
}

fn session_json(session: &AppSession) -> String {
    let process = match session.identity.process_id {
        pid if matches!(session.process_identity, ProcessIdentity::Known(_)) => pid.to_string(),
        _ => "null".to_string(),
    };
    format!(
        concat!(
            "{{\"endpoint_id\":{e},\"session_instance_id\":{s},\"process_id\":{p},",
            "\"display_name\":{d},\"process_identity\":{i},\"muted\":{m},",
            "\"volume\":{v},\"volume_valid\":{vv},\"active\":{a}}}"
        ),
        e = quote(&session.identity.endpoint_id),
        s = quote(&session.identity.session_instance_id),
        p = process,
        d = quote(&session.display_name),
        i = quote(&session.process_identity.to_string()),
        m = session.muted,
        v = format_volume(session.volume),
        vv = session.volume.is_finite(),
        a = session.active,
    )
}

/// Formats a volume as JSON, or `null` when the platform did not report one.
///
/// `NaN` and infinity are not valid JSON, so a non-finite volume becomes
/// `null` rather than a number. Emitting `0.0` would be a lie: an agent reading
/// this document cannot otherwise tell "this session is silent" apart from "the
/// platform never gave us a volume". Callers distinguish the two through
/// `volume_valid`, and mute state is carried by `muted` regardless.
fn format_volume(volume: f32) -> String {
    if volume.is_finite() {
        format!("{volume:.6}")
    } else {
        "null".to_string()
    }
}

/// Renders a volume for human-readable output.
///
/// Shares the "not reported" distinction with [`format_volume`]: an unreadable
/// volume is printed as `unknown`, never as `0`, so a reader cannot mistake a
/// platform failure for a session that is silent.
fn format_volume_text(volume: f32) -> String {
    if volume.is_finite() {
        format!("{volume:.6}")
    } else {
        "unknown".to_string()
    }
}

/// Escapes a string as a JSON string literal, including its quotes.
fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if control.is_control() => {
                out.push_str(&format!("\\u{:04x}", u32::from(control)))
            }
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// Renders an outcome as human-readable text.
///
/// Session instance ids are long and opaque, so they are printed on their own
/// line where they can be copied exactly: a user selecting a session must be
/// able to take the id verbatim into `--session`.
pub(crate) fn to_text(outcome: &SessionOutcome) -> String {
    match outcome {
        SessionOutcome::Sessions(sessions) => {
            if sessions.is_empty() {
                return "No application playback sessions on this endpoint.".to_string();
            }
            sessions
                .iter()
                .map(|session| {
                    format!(
                        "{name}  [{state}]\n  session: {id}\n  {proc}  muted={muted} volume={vol}",
                        name = session.display_name,
                        state = if session.active { "ACTIVE" } else { "INACTIVE" },
                        id = session.identity.session_instance_id,
                        proc = session.process_identity,
                        muted = session.muted,
                        vol = format_volume_text(session.volume),
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        SessionOutcome::State(session) => format!(
            "{state}: {name}\n  session: {id}\n  {proc}  muted={muted} volume={vol}",
            state = if session.muted { "MUTED" } else { "LIVE" },
            name = session.display_name,
            id = session.identity.session_instance_id,
            proc = session.process_identity,
            muted = session.muted,
            vol = format_volume_text(session.volume),
        ),
    }
}

/// Selects the one session matching `identity` from an observed set.
///
/// Returns an error naming the reason when the session is gone or when more
/// than one session shares the identity. Ambiguity is fatal rather than
/// resolved by picking a candidate: two sessions claiming one identity means
/// the platform state is not what this code assumes, and acting on either
/// could mute the wrong application.
pub(crate) fn select<'a>(
    sessions: &'a [AppSession],
    identity: &SessionIdentity,
) -> std::result::Result<&'a AppSession, String> {
    let mut matches = sessions.iter().filter(|session| {
        session.identity.endpoint_id == identity.endpoint_id
            && session.identity.session_instance_id == identity.session_instance_id
    });
    let first = matches.next().ok_or_else(|| {
        "That session no longer exists on this endpoint; run app-list again.".to_string()
    })?;
    if matches.next().is_some() {
        return Err(
            "More than one session matches that identity; refusing to act on an ambiguous target."
                .to_string(),
        );
    }
    // The process id recorded when the session was chosen is re-verified
    // here. A recycled PID means a different process now owns this session, so
    // the user's earlier selection no longer describes the same target.
    if first.identity.process_id != identity.process_id {
        return Err(format!(
            "That session is now owned by a different process (was pid {}, now pid {}); \
             run app-list again to reselect.",
            identity.process_id, first.identity.process_id
        ));
    }
    Ok(first)
}

/// The mute state a session action asks the platform for.
///
/// `None` means "read only": no state change is requested, so a read can never
/// be mistaken for a write.
pub(crate) fn desired_mute(action: SessionAction, observed: bool) -> Option<bool> {
    match action {
        SessionAction::Mute => Some(true),
        SessionAction::Unmute => Some(false),
        SessionAction::Toggle => Some(!observed),
        SessionAction::Devices | SessionAction::List | SessionAction::Status => None,
    }
}

/// Checks a requested mute state against what the platform read back.
///
/// This is the readback gate. A platform that accepted the call but did not
/// apply it must surface as an error here rather than as a reported success;
/// the meeting-isolation guarantee depends on never claiming a state that was
/// not observed.
pub(crate) fn verify_readback(
    action: SessionAction,
    requested: Option<bool>,
    read_back: bool,
) -> std::result::Result<(), String> {
    match requested {
        None => Ok(()),
        Some(wanted) if wanted == read_back => Ok(()),
        Some(wanted) => Err(format!(
            "{action} did not take effect: requested muted={wanted} but the platform reports muted={read_back}."
        )),
    }
}

/// Reports that this platform cannot control application sessions.
///
/// Kept in the core so every unsupported platform says the same thing and the
/// message can be asserted on from tests that run on any host. It must never
/// degrade into an empty session list, which would mean "we looked and found
/// none" — a different and misleading claim.
///
/// This is strictly a no-adapter message. Windows has the adapter, so a Windows
/// failure has a real cause and must not be reported as "unimplemented" — see
/// the COM and platform error helpers in the Windows adapter. It lives in the
/// core because the stub and the cross-platform tests both use it.
#[cfg_attr(windows, allow(dead_code))]
pub(crate) fn unsupported(action: SessionAction) -> String {
    format!(
        "{action} requires the Windows Core Audio session adapter; \
         application audio sessions are unimplemented on this platform."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| s.to_string()).collect()
    }

    fn session(id: &str, pid: u32, name: &str) -> AppSession {
        AppSession {
            identity: SessionIdentity {
                endpoint_id: "endpoint-1".to_string(),
                session_instance_id: id.to_string(),
                process_id: pid,
            },
            display_name: name.to_string(),
            process_identity: ProcessIdentity::Known(pid),
            muted: false,
            volume: 0.5,
            active: true,
        }
    }

    #[test]
    fn list_requires_an_endpoint_and_refuses_a_session() {
        assert!(parse_session(&args(&["app-list"])).is_err());
        assert!(parse_session(&args(&["app-list", "--endpoint", ""])).is_err());
        let (action, endpoint, session) =
            parse_session(&args(&["app-list", "--endpoint", "end"])).unwrap();
        assert_eq!(action, SessionAction::List);
        assert_eq!(endpoint.as_deref(), Some("end"));
        assert!(session.is_none());
        assert!(parse_session(&args(&["app-list", "--endpoint", "e", "--session", "s"])).is_err());
    }

    #[test]
    fn session_actions_require_both_selectors() {
        assert!(parse_session(&args(&["app-mute"])).is_err());
        assert!(parse_session(&args(&["app-mute", "--endpoint", "e"])).is_err());
        assert!(parse_session(&args(&["app-mute", "--session", "s"])).is_err());
        let (action, endpoint, session) =
            parse_session(&args(&["app-mute", "--endpoint", "e", "--session", "s"])).unwrap();
        assert_eq!(action, SessionAction::Mute);
        assert_eq!(endpoint.as_deref(), Some("e"));
        assert_eq!(session.as_deref(), Some("s"));
    }

    #[test]
    fn selectors_accept_either_order_and_reject_repeats_and_junk() {
        let (endpoint, session) = selectors(&args(&["--session", "s", "--endpoint", "e"])).unwrap();
        assert_eq!(endpoint.as_deref(), Some("e"));
        assert_eq!(session.as_deref(), Some("s"));
        // A repeated flag with a different value must not slip a second target
        // past the first parsed one.
        assert!(selectors(&args(&["--endpoint", "a", "--endpoint", "b"])).is_err());
        // The same value twice is harmless and accepted.
        assert!(selectors(&args(&["--endpoint", "a", "--endpoint", "a"])).is_ok());
        assert!(selectors(&args(&["--endpoint"])).is_err());
        assert!(selectors(&args(&["--bogus", "x"])).is_err());
        assert!(selectors(&args(&["--endpoint", "a\0b"])).is_err());
        assert!(selectors(&args(&["--endpoint", &"x".repeat(600)])).is_err());
    }

    #[test]
    fn unmapped_commands_are_rejected() {
        assert!(parse_session(&args(&["mute"])).is_err());
        assert!(parse_session(&args(&["app-volume"])).is_err());
        assert!(parse_session(&args(&[])).is_err());
    }

    #[test]
    fn actions_display_their_command_name() {
        // Rendered in help and in error messages, so the display name must be
        // the word the user actually types.
        assert_eq!(SessionAction::List.to_string(), "app-list");
        assert_eq!(SessionAction::Status.to_string(), "app-status");
        assert_eq!(SessionAction::Mute.to_string(), "app-mute");
        assert_eq!(SessionAction::Unmute.to_string(), "app-unmute");
        assert_eq!(SessionAction::Toggle.to_string(), "app-toggle");
    }

    #[test]
    fn select_matches_on_identity_and_rejects_stale_targets() {
        let sessions = vec![session("s1", 100, "Player"), session("s2", 200, "Meeting")];
        let identity = SessionIdentity {
            endpoint_id: "endpoint-1".to_string(),
            session_instance_id: "s2".to_string(),
            process_id: 200,
        };
        assert_eq!(
            select(&sessions, &identity).unwrap().display_name,
            "Meeting"
        );

        // Gone: an id that is no longer present is stale, not a default.
        let gone = SessionIdentity {
            session_instance_id: "missing".to_string(),
            ..identity.clone()
        };
        let error = select(&sessions, &gone).unwrap_err();
        assert!(error.contains("no longer exists"), "got: {error}");

        // Recycled PID: the session id matches but a different process owns it,
        // so the user's original selection no longer describes this target.
        let recycled = SessionIdentity {
            process_id: 999,
            ..identity
        };
        let error = select(&sessions, &recycled).unwrap_err();
        assert!(error.contains("different process"), "got: {error}");

        // A session on another endpoint is not the same session.
        let other = SessionIdentity {
            endpoint_id: "endpoint-2".to_string(),
            session_instance_id: "s2".to_string(),
            process_id: 200,
        };
        assert!(select(&sessions, &other).is_err());
    }

    #[test]
    fn select_refuses_an_ambiguous_match() {
        // Two sessions claiming one identity means our model of the platform is
        // wrong; acting on either could mute the wrong application.
        let sessions = vec![session("s1", 100, "Player"), session("s1", 100, "Player")];
        let identity = SessionIdentity {
            endpoint_id: "endpoint-1".to_string(),
            session_instance_id: "s1".to_string(),
            process_id: 100,
        };
        let error = select(&sessions, &identity).unwrap_err();
        assert!(error.contains("ambiguous"), "got: {error}");
    }

    #[test]
    fn read_only_actions_request_no_state_change() {
        assert_eq!(desired_mute(SessionAction::Status, false), None);
        assert_eq!(desired_mute(SessionAction::List, true), None);
        assert_eq!(desired_mute(SessionAction::Mute, false), Some(true));
        assert_eq!(desired_mute(SessionAction::Unmute, true), Some(false));
        assert_eq!(desired_mute(SessionAction::Toggle, false), Some(true));
        assert_eq!(desired_mute(SessionAction::Toggle, true), Some(false));
    }

    #[test]
    fn readback_must_match_or_fail() {
        assert!(verify_readback(SessionAction::Mute, Some(true), true).is_ok());
        // The platform accepted the call but did not apply it: an error, never
        // a reported success.
        let error = verify_readback(SessionAction::Mute, Some(true), false).unwrap_err();
        assert!(error.contains("did not take effect"), "got: {error}");
        // A read requests nothing, so any read back value is acceptable.
        assert!(verify_readback(SessionAction::Status, None, true).is_ok());
        assert!(verify_readback(SessionAction::Status, None, false).is_ok());
    }

    #[test]
    fn json_escapes_hostile_display_names() {
        let mut session = session("s1", 100, "quote\" back\\slash\nnewline");
        session.display_name = "quote\" back\\slash\nnewline".to_string();
        let json = to_json(&SessionOutcome::State(session));
        // A quote or backslash in a name must not alter the document shape.
        assert!(
            json.contains("quote\\\" back\\\\slash\\nnewline"),
            "got: {json}"
        );
        assert!(json.starts_with('{') && json.ends_with('}'));
    }

    #[test]
    fn json_emits_valid_numbers_for_non_finite_volume() {
        let mut session = session("s1", 100, "Player");
        session.volume = f32::NAN;
        let json = to_json(&SessionOutcome::State(session));
        assert!(!json.contains("NaN"), "NaN is not valid JSON: {json}");
    }

    #[test]
    fn json_reports_unknown_process_identity_as_null() {
        let mut session = session("s1", 100, "Player");
        session.process_identity = ProcessIdentity::Unknown;
        let json = to_json(&SessionOutcome::State(session));
        assert!(json.contains("\"process_id\":null"), "got: {json}");
        assert!(
            json.contains("\"process_identity\":\"unknown process\""),
            "got: {json}"
        );
    }

    #[test]
    fn json_wraps_enumerations_in_a_sessions_array() {
        let json = to_json(&SessionOutcome::Sessions(vec![session("s1", 1, "A")]));
        assert!(json.starts_with("{\"sessions\":["), "got: {json}");
        let empty = to_json(&SessionOutcome::Sessions(vec![]));
        assert_eq!(empty, "{\"sessions\":[]}");
    }

    #[test]
    fn text_output_states_mute_and_includes_the_full_session_id() {
        let session = session("s1", 100, "Player");
        let text = to_text(&SessionOutcome::State(session));
        assert!(text.contains("LIVE: Player"), "got: {text}");
        // The id must be copyable verbatim into --session.
        assert!(text.contains("session: s1"), "got: {text}");
        let empty = to_text(&SessionOutcome::Sessions(vec![]));
        assert!(
            empty.contains("No application playback sessions"),
            "got: {empty}"
        );
    }

    #[test]
    fn unsupported_platforms_say_so_explicitly() {
        // "Unimplemented" must never be reported as an empty session list,
        // which would falsely mean "we looked and found none".
        let message = unsupported(SessionAction::List);
        assert!(message.contains("unimplemented"), "got: {message}");
        assert!(message.contains("Windows"), "got: {message}");
    }
}
