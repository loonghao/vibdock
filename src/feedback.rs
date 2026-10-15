//! Desktop status feedback contracts.
//!
//! This module is deliberately platform-free: it defines what a status
//! indicator may show and derives that purely from an action the service has
//! already completed. Painting a window is the platform adapter's job
//! (`feedback_windows`), never this module's.
//!
//! The honesty rule this module exists to enforce: a status frame reports
//! success only when a verified readback produced it. Registration, a bound
//! action being armed, or a shortcut being dispatched are none of those, so
//! none of them can ever surface here as `Live` or `Muted`.

/// Corner of the screen the indicator occupies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Corner {
    /// Bottom-right, above the notification area.
    #[default]
    BottomRight,
    /// Bottom-left.
    BottomLeft,
    /// Top-right.
    TopRight,
    /// Top-left.
    TopLeft,
}

/// Opt-in desktop indicator settings.
///
/// `enabled` defaults to `false`. The indicator is an explicit opt-in because
/// it draws a window on the user's desktop; nothing should appear there
/// without the user having asked for it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FeedbackConfig {
    /// Whether to show the indicator at all. Default `false`.
    pub enabled: bool,
    /// Where to place the indicator.
    pub corner: Corner,
    /// Human-readable name of the selected target, kept visible so the user
    /// can confirm which device an action applied to.
    pub target_label: String,
}

/// Environment variable that turns the indicator on.
///
/// The feature is opt-in, so there must be a real way to opt in. This is the
/// smallest entry point: one documented variable, checked once at startup,
/// with no new configuration framework.
pub const ENABLED_ENV: &str = "VIBDOCK_OVERLAY";

/// Values of [`ENABLED_ENV`] that enable the indicator.
///
/// Compared case-insensitively so `1`, `on`, and `true` all work; anything
/// else — including an unset or empty variable — leaves it off.
const ENABLED_VALUES: [&str; 4] = ["1", "on", "true", "yes"];

/// Corner names accepted by the placement variable.
const CORNER_VALUES: [(&str, Corner); 4] = [
    ("bottom-right", Corner::BottomRight),
    ("bottom-left", Corner::BottomLeft),
    ("top-right", Corner::TopRight),
    ("top-left", Corner::TopLeft),
];

/// Reads the indicator's opt-in state from the environment.
///
/// Pure with respect to its argument so it can be verified without touching
/// the process environment: tests pass an explicit value instead.
fn enabled_from(raw: Option<&str>) -> bool {
    let Some(raw) = raw else {
        // Unset: the default, and the state the feature ships in.
        return false;
    };
    ENABLED_VALUES.contains(&raw.trim().to_ascii_lowercase().as_str())
}

/// Reads the configured corner from the environment.
///
/// An unrecognised name falls back to the default corner rather than failing
/// startup: placement is a preference, not a precondition.
fn corner_from(raw: Option<&str>) -> Corner {
    let Some(raw) = raw else {
        return Corner::default();
    };
    let cleaned = raw.trim().to_ascii_lowercase();
    CORNER_VALUES
        .iter()
        .find(|(name, _)| *name == cleaned)
        .map(|(_, corner)| *corner)
        .unwrap_or_default()
}

/// Resolves the indicator's settings from the process environment.
///
/// The only reachable opt-in path for this slice. Both lookups take explicit
/// values so the resolution is testable without mutating global state.
pub fn config_from_env(enabled_raw: Option<&str>, corner_raw: Option<&str>) -> FeedbackConfig {
    FeedbackConfig {
        enabled: enabled_from(enabled_raw),
        corner: corner_from(corner_raw),
        target_label: String::new(),
    }
}

/// Resolves the settings from this process's actual environment.
///
/// Thin wrapper over [`config_from_env`] so the accept loop has one call and
/// every rule above stays testable.
pub fn config_from_process_env() -> FeedbackConfig {
    config_from_env(
        std::env::var(ENABLED_ENV).ok().as_deref(),
        std::env::var("VIBDOCK_OVERLAY_CORNER").ok().as_deref(),
    )
}

/// How the last completed action stands.
///
/// Every variant carries both an icon and text. Colour alone is never the
/// signal: a user who cannot distinguish the colours must still be able to read
/// the state, and a status conveyed only by hue is invisible to them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    /// Nothing has been observed yet in this service instance.
    Idle,
    /// A verified readback reported the microphone live.
    Live,
    /// A verified readback reported the microphone muted.
    Muted,
    /// The action was attempted and reported a real failure.
    Failed,
    /// The state could not be established.
    Unknown,
}

impl StatusKind {
    /// A short, non-colour glyph for this state.
    pub fn icon(self) -> &'static str {
        match self {
            StatusKind::Idle => "-",
            StatusKind::Live => "[LIVE]",
            StatusKind::Muted => "[MUTED]",
            StatusKind::Failed => "[!]",
            StatusKind::Unknown => "[?]",
        }
    }

    /// True only for a state produced by a verified platform readback.
    ///
    /// Used so a caller can tell "we know the microphone is live" from "an
    /// action happened and we hope it worked".
    pub fn is_verified(self) -> bool {
        matches!(self, StatusKind::Live | StatusKind::Muted)
    }
}

/// One renderable status frame.
///
/// Derived from an already-completed action. Constructing a frame never
/// performs an action and never queries the platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusFrame {
    /// How the last completed action stands.
    pub kind: StatusKind,
    /// Non-colour glyph for the state.
    pub icon: &'static str,
    /// Full text describing the state.
    pub text: String,
    /// The selected target, kept visible.
    pub target: String,
    /// The action sequence this frame describes.
    pub sequence: u64,
}

/// The last completed action, exactly as the service recorded it.
///
/// This mirrors the service's own `last_action` view rather than re-deriving
/// it, so the indicator and the API cannot disagree about what happened.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LastActionView {
    /// Number of completed actions in this service instance. Zero means none.
    pub sequence: u64,
    /// Verified state read back from the platform, if the action succeeded.
    pub state: Option<String>,
    /// Real failure message, if the action failed.
    pub error: Option<String>,
}

/// Derives a status frame from a completed action.
///
/// This is a pure function of recorded state. Because it renders only the
/// latest completed action, it coalesces *display* updates — rapid actions
/// repaint the one indicator instead of stacking windows — while never
/// dropping an action: every action already advanced `sequence` before this
/// runs, and the sequence is carried into the frame.
pub fn frame_from(last_action: &LastActionView, config: &FeedbackConfig) -> StatusFrame {
    let (kind, text) = classify(last_action);
    StatusFrame {
        kind,
        icon: kind.icon(),
        text,
        target: config.target_label.clone(),
        sequence: last_action.sequence,
    }
}

/// Maps a completed action onto its state and text.
///
/// The order of these checks is the contract. An error is checked before a
/// stale state so a failed action can never be displayed as the previous
/// success: the service clears `state` on failure, but re-checking here means
/// the display stays honest even if a caller hands us both.
fn classify(last_action: &LastActionView) -> (StatusKind, String) {
    if last_action.sequence == 0 {
        // Nothing has been observed. This is not success and not failure: it
        // is the absence of evidence, and must read as such.
        return (StatusKind::Idle, "No action yet".to_string());
    }
    if let Some(error) = &last_action.error {
        return (StatusKind::Failed, format!("Action failed: {error}"));
    }
    match last_action.state.as_deref() {
        Some("LIVE") => (StatusKind::Live, "Microphone live".to_string()),
        Some("MUTED") => (StatusKind::Muted, "Microphone muted".to_string()),
        // A completed action whose state we cannot name is unknown, never
        // silently treated as a success the user would trust.
        _ => (StatusKind::Unknown, "Status unknown".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> FeedbackConfig {
        FeedbackConfig {
            enabled: true,
            corner: Corner::BottomRight,
            target_label: "Shure MV7".to_string(),
        }
    }

    #[test]
    fn the_indicator_is_opt_in_and_defaults_off() {
        // An explicit opt-in: nothing draws on the user's desktop unless they
        // asked for it.
        assert!(!FeedbackConfig::default().enabled);
    }

    #[test]
    fn no_observed_action_is_idle_and_never_success() {
        let frame = frame_from(&LastActionView::default(), &config());
        assert_eq!(frame.kind, StatusKind::Idle);
        assert_eq!(frame.sequence, 0);
        assert!(
            !frame.kind.is_verified(),
            "an unobserved state must not read as verified"
        );
    }

    #[test]
    fn verified_readbacks_report_live_and_muted() {
        let live = frame_from(
            &LastActionView {
                sequence: 1,
                state: Some("LIVE".to_string()),
                error: None,
            },
            &config(),
        );
        assert_eq!(live.kind, StatusKind::Live);
        assert!(live.kind.is_verified());
        assert_eq!(live.text, "Microphone live");

        let muted = frame_from(
            &LastActionView {
                sequence: 2,
                state: Some("MUTED".to_string()),
                error: None,
            },
            &config(),
        );
        assert_eq!(muted.kind, StatusKind::Muted);
        assert!(muted.kind.is_verified());
        assert_eq!(muted.text, "Microphone muted");
    }

    #[test]
    fn a_failed_action_is_never_shown_as_the_previous_success() {
        // Both fields set at once is not a state the service produces, but the
        // display must still not claim success from it.
        let frame = frame_from(
            &LastActionView {
                sequence: 3,
                state: Some("LIVE".to_string()),
                error: Some("endpoint disconnected".to_string()),
            },
            &config(),
        );
        assert_eq!(frame.kind, StatusKind::Failed);
        assert!(!frame.kind.is_verified());
        assert!(frame.text.contains("endpoint disconnected"));
    }

    #[test]
    fn an_unnamed_state_is_unknown_not_success() {
        let frame = frame_from(
            &LastActionView {
                sequence: 1,
                state: Some("SOMETHING_ELSE".to_string()),
                error: None,
            },
            &config(),
        );
        assert_eq!(frame.kind, StatusKind::Unknown);
        assert!(!frame.kind.is_verified());
    }

    #[test]
    fn frames_carry_the_sequence_so_display_updates_never_drop_actions() {
        // The sequence is the evidence that an action occurred; rendering only
        // the latest frame coalesces paint work, not actions.
        let frame = frame_from(
            &LastActionView {
                sequence: 7,
                state: Some("MUTED".to_string()),
                error: None,
            },
            &config(),
        );
        assert_eq!(frame.sequence, 7);
    }

    #[test]
    fn the_selected_target_stays_visible_in_every_frame() {
        let frame = frame_from(&LastActionView::default(), &config());
        assert_eq!(frame.target, "Shure MV7");
    }

    #[test]
    fn the_indicator_is_off_unless_the_environment_opts_in() {
        // The default must stay off: an unset, empty, or unrecognised value
        // never draws a window on the user's desktop.
        for raw in [
            None,
            Some(""),
            Some("0"),
            Some("off"),
            Some("false"),
            Some("no"),
        ] {
            assert!(
                !config_from_env(raw, None).enabled,
                "{raw:?} must not enable the indicator"
            );
        }
        assert!(!FeedbackConfig::default().enabled);
    }

    #[test]
    fn documented_values_enable_the_indicator() {
        // A reachable opt-in: each accepted spelling actually turns it on, and
        // case and surrounding whitespace must not matter.
        for raw in ["1", "on", "true", "yes", "ON", " True ", "YES"] {
            assert!(
                config_from_env(Some(raw), None).enabled,
                "{raw:?} must enable the indicator"
            );
        }
    }

    #[test]
    fn every_corner_name_resolves_to_its_corner() {
        for (name, expected) in [
            ("bottom-right", Corner::BottomRight),
            ("bottom-left", Corner::BottomLeft),
            ("top-right", Corner::TopRight),
            ("top-left", Corner::TopLeft),
        ] {
            assert_eq!(
                config_from_env(Some("1"), Some(name)).corner,
                expected,
                "{name} resolved to the wrong corner"
            );
        }
    }

    #[test]
    fn an_unknown_corner_falls_back_to_the_default() {
        // Placement is a preference: an unrecognised name must not prevent the
        // indicator from starting.
        assert_eq!(
            config_from_env(Some("1"), Some("middle")).corner,
            Corner::default()
        );
        assert_eq!(config_from_env(Some("1"), None).corner, Corner::default());
    }

    #[test]
    fn the_opt_in_variable_has_a_stable_documented_name() {
        // The documented entry point. Changing it silently would strand users
        // who already set it.
        assert_eq!(ENABLED_ENV, "VIBDOCK_OVERLAY");
    }

    #[test]
    fn every_state_carries_both_text_and_a_non_colour_icon() {
        // Accessibility: the state must be readable without distinguishing
        // colours, so both channels are always populated.
        for kind in [
            StatusKind::Idle,
            StatusKind::Live,
            StatusKind::Muted,
            StatusKind::Failed,
            StatusKind::Unknown,
        ] {
            assert!(!kind.icon().is_empty(), "{kind:?} has no icon");
        }
        for view in [
            LastActionView::default(),
            LastActionView {
                sequence: 1,
                state: Some("LIVE".to_string()),
                error: None,
            },
            LastActionView {
                sequence: 1,
                state: None,
                error: Some("boom".to_string()),
            },
            LastActionView {
                sequence: 1,
                state: None,
                error: None,
            },
        ] {
            let frame = frame_from(&view, &config());
            assert!(!frame.text.is_empty());
            assert!(!frame.icon.is_empty());
        }
    }
}
