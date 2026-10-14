//! Codex (ChatGPT desktop) voice integration probe.
//!
//! This probe is a static declaration derived from published documentation. It
//! does not inspect an installed client, so no result here implies the local
//! environment was examined.
//!
//! Evidence basis: the ChatGPT voice documentation describes a user-configurable
//! voice chat hotkey under Settings > Voice, and distinguishes live voice
//! conversation from dictation. It documents no external mute API and no
//! dedicated mute shortcut. Absence from the documentation is not proof that a
//! control is absent, so such capabilities are reported as
//! [`Support::Unknown`], never as [`Support::Unsupported`].
//!
//! Scope is kept separated from the planned Windows application-playback-session
//! adapter: muting Codex output through the OS audio session is a different
//! capability from any mute control Codex itself might expose.

use crate::capabilities::{Capability, Finding, Probe, Support};

pub struct Codex;

const SOURCE: &str = "https://learn.chatgpt.com/docs/features/voice";

const NOT_INSPECTED: &str = "no installed-client inspection performed";

impl Probe for Codex {
    fn integration(&self) -> &'static str {
        "codex"
    }

    fn probe(&self, capability: Capability) -> Finding {
        let (support, detail) = match capability {
            Capability::Show => (
                Support::Unknown,
                format!("{SOURCE} does not document a programmatic show API. Undocumented, not disproven; {NOT_INSPECTED}."),
            ),
            Capability::StartVoice => (
                Support::Unverified,
                format!("{SOURCE} documents a configurable voice chat hotkey under Settings > Voice. Read the configured hotkey from the installed client; do not hard-code a wake-up shortcut. {NOT_INSPECTED}."),
            ),
            Capability::EndVoice => (
                Support::Unknown,
                "Documented as live voice conversation distinct from dictation. End-of-call control is undocumented, not disproven.".to_string(),
            ),
            Capability::AudioIdentity => (
                Support::Unknown,
                "Target audio identity requires live readback from the installed client. No public API is documented.".to_string(),
            ),
            Capability::InputMute => (
                Support::Unknown,
                "Microphone input mute is handled by the device endpoint rather than by this integration. Codex-side input mute is undocumented, not disproven.".to_string(),
            ),
            Capability::AppMute => (
                Support::Unknown,
                format!("{SOURCE} documents no external mute API and no dedicated mute shortcut. Undocumented is not absent; a Codex-native output mute remains unverified rather than unsupported."),
            ),
            Capability::PlaybackSessionMute => (
                Support::Unknown,
                "Windows application-playback-session mute is a separate planned adapter, out of scope for this integration. Not conflated with any Codex-native mute control.".to_string(),
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
    fn undocumented_mute_is_unknown_not_unsupported() {
        let report = inspect(&Codex);
        for capability in [Capability::InputMute, Capability::AppMute] {
            let finding = report
                .findings
                .iter()
                .find(|f| f.capability == capability)
                .unwrap();
            assert_ne!(
                finding.support,
                Support::Unsupported,
                "{} is undocumented, not inspected and disproven",
                capability.as_str()
            );
            assert_eq!(
                finding.support,
                Support::Unknown,
                "{} must be reported as never inspected",
                capability.as_str()
            );
        }
    }

    #[test]
    fn codex_mute_is_separated_from_playback_session_mute() {
        let report = inspect(&Codex);
        let app_mute = report
            .findings
            .iter()
            .find(|f| f.capability == Capability::AppMute)
            .unwrap()
            .detail
            .clone();
        let playback = report
            .findings
            .iter()
            .find(|f| f.capability == Capability::PlaybackSessionMute)
            .unwrap()
            .detail
            .clone();
        assert!(playback.contains("separate planned adapter"));
        assert!(app_mute.contains("Codex-native"));
    }

    #[test]
    fn details_do_not_claim_an_inspection_happened() {
        let report = inspect(&Codex);
        for finding in &report.findings {
            let lower = finding.detail.to_lowercase();
            for phrase in [
                "we inspected",
                "the installed client was inspected",
                "confirmed absent",
            ] {
                assert!(
                    !lower.contains(phrase),
                    "{} implies an inspection that did not happen",
                    finding.capability.as_str()
                );
            }
        }
    }
}
