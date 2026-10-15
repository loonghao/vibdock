//! Non-Windows counterpart to the session adapter: unsupported, not empty.
//!
//! Every application-session command fails here with an explicit
//! "unimplemented" error. Returning an empty session list would mean "we
//! looked and found none", which is a different and misleading claim — a
//! caller could not tell it apart from a real enumeration of an endpoint that
//! genuinely has no sessions.
//!
//! This module is also what keeps the platform-independent contract in
//! `app_audio` live on every platform: the selector and readback rules are
//! exercised here rather than only in tests, so the shared types have a
//! consumer even where the adapter does not exist.

use crate::app_audio::{
    self, AppSession, ProcessIdentity, SessionAction, SessionIdentity, SessionOutcome,
};

/// Executes an application-session action where no adapter exists.
///
/// Always an error. The identity rules from the core are applied first so the
/// selectors a user supplied are validated on every platform: a malformed
/// session id is reported as such rather than as "unimplemented", because the
/// user can act on the former.
pub(crate) fn execute(
    action: SessionAction,
    endpoint_id: &str,
    session_id: Option<&str>,
) -> std::result::Result<SessionOutcome, String> {
    // Session control is playback-only on every platform, including where it
    // is unimplemented: stating that here keeps the scope from widening.
    debug_assert_eq!(app_audio::FLOW, app_audio::SessionFlow::Render);
    // Validate the selector shape before reporting unsupported, so the error
    // names the thing the user can actually fix.
    if action != SessionAction::List && session_id.is_none_or(str::is_empty) {
        return Err(session_selector_error());
    }
    if endpoint_id.is_empty() {
        return Err("Specify a render endpoint with --endpoint <endpoint-id>.".into());
    }
    // Run the shared identity and readback rules against an empty observed
    // set. That is the honest model of a platform that cannot look: nothing
    // was found, and the reason is reported separately rather than as a
    // successful empty result. Going through the shared functions here also
    // means the contract is exercised on every platform, not only in tests.
    let observed: Vec<AppSession> = Vec::new();
    if action != SessionAction::List {
        let identity = SessionIdentity {
            endpoint_id: endpoint_id.to_string(),
            session_instance_id: session_id.unwrap_or_default().to_string(),
            // No process can be resolved, so this stays Unknown rather than
            // defaulting to a plausible-looking pid.
            process_id: match ProcessIdentity::Unknown {
                ProcessIdentity::Known(pid) => pid,
                ProcessIdentity::Unknown => 0,
            },
        };
        app_audio::select(&observed, &identity)?;
        // Unreachable with an empty set, but the readback gate is applied
        // anyway so the rule is enforced on every path: a requested state is
        // only ever reported after the platform confirms it.
        let requested = app_audio::desired_mute(action, false);
        app_audio::verify_readback(action, requested, false)?;
    }
    // Name what was asked for alongside why it is unavailable. The request is
    // described from the selectors rather than from the empty observed set, so
    // the error says which session was targeted instead of describing nothing.
    let looked_for = format!(
        "endpoint {endpoint_id} session {}",
        session_id.unwrap_or_default()
    );
    Err(format!(
        "{} (looked for: {looked_for})",
        app_audio::unsupported(action)
    ))
}

/// The error used when a session action is missing its session selector.
fn session_selector_error() -> String {
    "Specify a session instance id with --session <session-instance-id>; run app-list to enumerate."
        .to_string()
}

/// Lists render endpoints where no adapter exists.
///
/// Fails rather than returning an empty list, for the same reason as
/// [`execute`]: "cannot look" must not be reported as "looked, found none".
pub(crate) fn render_endpoints() -> crate::Result<Vec<(String, String)>> {
    Err(app_audio::unsupported(SessionAction::Devices).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_reports_unsupported_not_empty() {
        // The whole point of the stub: no command may ever return a
        // successful empty enumeration, which would be indistinguishable
        // from a real endpoint with no sessions.
        for action in [
            SessionAction::List,
            SessionAction::Status,
            SessionAction::Mute,
            SessionAction::Unmute,
            SessionAction::Toggle,
        ] {
            let result = execute(action, "endpoint", Some("session"));
            assert!(result.is_err(), "{action} must not succeed here");
            let error = result.unwrap_err();
            assert!(error.contains("unimplemented"), "{action}: {error}");
        }
    }

    #[test]
    fn missing_selectors_are_reported_before_unsupported() {
        // A fixable user error must not be hidden behind "unimplemented".
        let error = execute(SessionAction::Mute, "endpoint", None).unwrap_err();
        assert!(error.contains("--session"), "got: {error}");
        let error = execute(SessionAction::Mute, "", Some("s")).unwrap_err();
        assert!(error.contains("--endpoint"), "got: {error}");
    }

    #[test]
    fn enumeration_never_succeeds_even_for_a_valid_endpoint() {
        let result = execute(SessionAction::List, "endpoint", None);
        assert!(result.is_err(), "app-list must not report an empty success");
        assert!(render_endpoints().is_err());
    }

    /// Builds a session with a resolved owning process, for render checks.
    fn sample_session() -> AppSession {
        AppSession {
            identity: SessionIdentity {
                endpoint_id: "endpoint".to_string(),
                session_instance_id: "session".to_string(),
                process_id: 42,
            },
            display_name: "Player".to_string(),
            process_identity: ProcessIdentity::Known(42),
            muted: false,
            volume: 1.0,
            active: true,
        }
    }

    #[test]
    fn both_outcome_shapes_render_on_this_platform() {
        // Both variants must render, so neither can be a shape the code can
        // construct but the renderer silently mishandles.
        let listed = app_audio::to_text(&SessionOutcome::Sessions(vec![sample_session()]));
        assert!(listed.contains("Player"), "got: {listed}");
        let state = app_audio::to_text(&SessionOutcome::State(sample_session()));
        assert!(state.contains("LIVE"), "got: {state}");
    }
}
