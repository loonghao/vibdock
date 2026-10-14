//! Codex (ChatGPT desktop) voice integration probe.
//!
//! Evidence basis: the ChatGPT voice documentation describes a user-configurable
//! voice chat hotkey under Settings > Voice, and distinguishes live voice
//! conversation from dictation. It documents no external mute API and no
//! dedicated mute shortcut, so mute capabilities are reported as unverified
//! until confirmed against the installed client. This module declares what is
//! known and refuses to infer private control APIs from UI presence.

use crate::capabilities::{Capability, Finding, Probe, Support};

pub struct Codex;

const SOURCE: &str = "https://learn.chatgpt.com/docs/features/voice";

impl Probe for Codex {
    fn integration(&self) -> &'static str {
        "codex"
    }

    fn probe(&self, capability: Capability) -> Finding {
        let (support, detail) = match capability {
            Capability::Show => (
                Support::Unverified,
                format!("No installed-client window or process evidence captured yet. {SOURCE} does not document a programmatic show API."),
            ),
            Capability::StartVoice => (
                Support::Unverified,
                format!("{SOURCE} documents a configurable voice chat hotkey under Settings > Voice. Read the configured hotkey from the installed client; do not hard-code a wake-up shortcut."),
            ),
            Capability::EndVoice => (
                Support::Unverified,
                "Documented as live voice conversation distinct from dictation. End-of-call control is not documented and needs installed-client verification.".to_string(),
            ),
            Capability::AudioIdentity => (
                Support::Unverified,
                "Target audio identity requires live readback from the installed client. No public API is documented.".to_string(),
            ),
            Capability::InputMute => (
                Support::Unverified,
                "Microphone input mute is handled by the device endpoint, not by this integration. Scope must be confirmed per target.".to_string(),
            ),
            Capability::AppMute => (
                Support::Unsupported,
                format!("{SOURCE} documents no external mute API and no dedicated mute shortcut. Not inferable from UI presence; per-app output mute is not a documented Codex capability."),
            ),
        };
        Finding {
            capability,
            support,
            detail,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::{inspect, Support};

    #[test]
    fn no_capability_is_claimed_verified_without_installed_client_evidence() {
        let report = inspect(&Codex);
        for finding in &report.findings {
            assert_ne!(
                finding.support,
                Support::Verified,
                "{} claimed verified without live evidence",
                finding.capability.as_str()
            );
            assert!(!finding.detail.is_empty());
        }
        assert!(report.verified().is_empty());
        assert!(report.has_unverified());
    }

    #[test]
    fn mute_capabilities_are_not_presented_as_supported() {
        let report = inspect(&Codex);
        for capability in [Capability::InputMute, Capability::AppMute] {
            let finding = report
                .findings
                .iter()
                .find(|f| f.capability == capability)
                .unwrap();
            assert_ne!(
                finding.support,
                Support::Verified,
                "{} must not be reported as working",
                capability.as_str()
            );
        }
    }

    #[test]
    fn app_mute_is_unsupported_not_unverified() {
        let report = inspect(&Codex);
        let finding = report
            .findings
            .iter()
            .find(|f| f.capability == Capability::AppMute)
            .unwrap();
        assert_eq!(finding.support, Support::Unsupported);
    }
}
