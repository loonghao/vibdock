//! Codex (ChatGPT desktop) voice integration probe.
//!
//! This probe is a static declaration derived from published documentation. It
//! does not inspect an installed client, so no result here implies the local
//! environment was examined.
//!
//! Evidence basis: the ChatGPT voice documentation describes a user-configurable
//! voice chat hotkey under Settings > Voice, a manual "Stop voice chat" control,
//! and distinguishes live voice conversation from dictation. It documents no
//! external mute API and no dedicated mute shortcut. Absence from the
//! documentation is not proof that a control is absent, so such capabilities are
//! reported as [`Support::Unknown`], never as [`Support::Unsupported`].
//!
//! Grade axis, applied uniformly: a capability the documentation describes but
//! no live client confirmed is [`Support::Unverified`]; one the documentation
//! does not describe at all is [`Support::Unknown`]. No entry here implies an
//! installed client was inspected.
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
                format!("{SOURCE} documents a manual UI stop only: \"Select Stop voice chat when you finish.\" It documents no configurable hotkey and no external API for ending a call, so external automation of voice end remains unproven rather than disproven. {NOT_INSPECTED}."),
            ),
            Capability::AudioIdentity => (
                Support::Unknown,
                format!("Target audio identity requires live readback from the installed client. No public API is documented. {NOT_INSPECTED}."),
            ),
            Capability::InputMute => (
                Support::Unknown,
                format!("Microphone input mute is handled by the device endpoint rather than by this integration. Codex-side input mute is undocumented, not disproven. {NOT_INSPECTED}."),
            ),
            Capability::AppMute => (
                Support::Unknown,
                format!("{SOURCE} documents no external mute API and no dedicated mute shortcut. Undocumented is not absent; a Codex-native output mute remains unverified rather than unsupported. {NOT_INSPECTED}."),
            ),
            Capability::PlaybackSessionMute => (
                Support::Unknown,
                format!("Windows application-playback-session mute is a separate planned adapter, out of scope for this integration. Not conflated with any Codex-native mute control. {NOT_INSPECTED}."),
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
            // Pin the exact grade: Unknown, and neither of the two it could
            // drift into. Unsupported would claim an inspection that disproved
            // the capability; Unverified would claim the documentation
            // describes a mute control, which it does not.
            assert_eq!(
                finding.support,
                Support::Unknown,
                "{} must be reported as never inspected",
                capability.as_str()
            );
        }
    }

    #[test]
    fn end_voice_matches_the_source_it_cites() {
        let report = inspect(&Codex);
        let finding = report
            .findings
            .iter()
            .find(|f| f.capability == Capability::EndVoice)
            .unwrap();
        assert_eq!(
            finding.support,
            Support::Unknown,
            "the Unknown grade must be scoped to external automation"
        );
        // The source documents a manual UI stop, so the detail must admit it
        // exists while scoping the Unknown grade to external automation.
        assert!(
            finding.detail.contains("Stop voice chat"),
            "must acknowledge the documented manual UI stop"
        );
        assert!(
            finding.detail.contains("no external API"),
            "must scope the Unknown grade to external automation"
        );
        assert!(
            !finding
                .detail
                .contains("End-of-call control is undocumented"),
            "contradicted by the cited source"
        );
    }

    #[test]
    fn documented_capabilities_are_not_graded_unknown() {
        let report = inspect(&Codex);
        // StartVoice is described by the documentation, so it may not sit at
        // Unknown. This pins the documented -> at least Unverified half of the
        // grade axis; unknown_is_distinct_from_unsupported pins the other half.
        let finding = report
            .findings
            .iter()
            .find(|f| f.capability == Capability::StartVoice)
            .unwrap();
        assert_ne!(
            finding.support,
            Support::Unknown,
            "start_voice is documented, so it cannot be graded as having no evidence"
        );
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
