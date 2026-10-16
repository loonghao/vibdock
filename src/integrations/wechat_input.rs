//! WeChat Input (微信输入法 / WeType) voice integration probe.
//!
//! This probe is a static declaration. It does not inspect an installed client,
//! so no result here implies the local environment was examined. Nothing in the
//! [`Probe`] output may describe the machine it runs on.
//!
//! Evidence basis differs from [`crate::integrations::codex::Codex`] in one
//! important way: no official source could be retrieved for WeChat Input. The
//! official site <https://shurufa.weixin.qq.com/> did not resolve, and no
//! official-domain documentation established a voice shortcut or an automation
//! API. Under the rule stated in [`crate::capabilities`], a probe that works
//! purely from documentation must report [`Support::Unknown`] for anything the
//! documentation does not describe. Here there is no documentation at all, so
//! every capability sits at [`Support::Unknown`].
//!
//! A dated, read-only snapshot was taken on one developer host, but it is
//! historical single-host evidence held in `docs/integrations/wechat-input.md`,
//! never in this module: it says what that one machine looked like on
//! 2026-10-15 and cannot be replayed as a result on any other machine. It
//! carries no grade either way. See that document for the record and for what
//! it does not establish.
//!
//! Grade axis, applied uniformly: with no retrievable official source, nothing
//! is graded [`Support::Unverified`] (which would claim documentation describes
//! it) and nothing is graded [`Support::Unsupported`] (which would claim an
//! inspection disproved it). Every entry is [`Support::Unknown`].

use crate::capabilities::{Capability, Finding, Probe, Support};

pub struct WeChatInput;

/// Official source consulted. Unreachable at the time of writing; recorded so
/// the gap is traceable rather than silently omitted.
const SOURCE: &str = "https://shurufa.weixin.qq.com/";

const NOT_INSPECTED: &str = "no installed-client capability inspection performed";

/// States that no local client was examined, without naming any machine,
/// version, or process: this text reaches every runtime capability report and
/// must stay true on any host.
const NO_LOCAL_STATE: &str =
    "no installed client is detected or described here; this declaration makes no claim about whether WeChat Input is present or running on this machine";

impl Probe for WeChatInput {
    fn integration(&self) -> &'static str {
        "wechat_input"
    }

    fn probe(&self, capability: Capability) -> Finding {
        let detail = match capability {
            Capability::Show => format!(
                "{SOURCE} was unreachable and no official-domain documentation described a \
                 programmatic show or focus API. Undocumented, not disproven; {NOT_INSPECTED}. \
                 {NO_LOCAL_STATE}."
            ),
            Capability::StartVoice => format!(
                "A configured trigger must be supplied by the user from their own client \
                 settings; no trigger may be hard-coded, and no default is asserted here. \
                 {SOURCE} was unreachable and no official source documented a default voice \
                 shortcut or an external start API, so neither a specific trigger nor \
                 user-configurability is established by this declaration. Undocumented, not \
                 disproven; {NOT_INSPECTED}. {NO_LOCAL_STATE}."
            ),
            Capability::EndVoice => format!(
                "{SOURCE} was unreachable and no official source documented how an in-progress \
                 voice session ends, or any external API for ending one. Undocumented, not \
                 disproven; {NOT_INSPECTED}. {NO_LOCAL_STATE}."
            ),
            Capability::AudioIdentity => format!(
                "Target audio identity requires live readback from an installed client. {SOURCE} \
                 was unreachable and no public API is documented. Undocumented, not disproven; \
                 {NOT_INSPECTED}. {NO_LOCAL_STATE}."
            ),
            Capability::InputMute => format!(
                "Microphone input mute is handled by the device endpoint rather than by this \
                 integration. {SOURCE} was unreachable and a WeType-native input mute is \
                 undocumented, not disproven. {NOT_INSPECTED}. {NO_LOCAL_STATE}."
            ),
            Capability::AppMute => format!(
                "{SOURCE} was unreachable and no official source documented an output-mute API \
                 or a dedicated mute shortcut for this client. Undocumented is not absent; a \
                 WeType-native output mute remains unverified rather than unsupported. \
                 {NOT_INSPECTED}. {NO_LOCAL_STATE}."
            ),
            Capability::PlaybackSessionMute => format!(
                "Windows application-playback-session mute is a separate Windows session CLI \
                 under native acceptance hold, out of scope for this integration and carrying \
                 no WeType action integration; {SOURCE} was unreachable and no official source \
                 described a WeType-native equivalent. Undocumented, not disproven. Not \
                 conflated with any WeType-native mute control. {NOT_INSPECTED}. \
                 {NO_LOCAL_STATE}."
            ),
        };
        Finding {
            capability,
            support: Support::Unknown,
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
        let report = inspect(&WeChatInput);
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

    /// With no retrievable official source, nothing may be graded Unverified:
    /// that grade asserts documentation describes the capability.
    #[test]
    fn nothing_is_graded_unverified_without_a_retrieved_source() {
        let report = inspect(&WeChatInput);
        for finding in &report.findings {
            assert_ne!(
                finding.support,
                Support::Unverified,
                "{} graded unverified although no official source was retrieved",
                finding.capability.as_str()
            );
        }
    }

    /// No inspection of a control surface happened, so a private one is not
    /// disproven. Unsupported claims a disproof, so it must not appear.
    #[test]
    fn absent_control_surface_is_unknown_not_unsupported() {
        let report = inspect(&WeChatInput);
        for finding in &report.findings {
            assert_ne!(
                finding.support,
                Support::Unsupported,
                "{} must not claim an inspection disproved the capability",
                finding.capability.as_str()
            );
        }
        assert!(report.has_unknown());
        assert_eq!(
            report
                .findings
                .iter()
                .filter(|f| f.support == Support::Unknown)
                .count(),
            Capability::ALL.len()
        );
    }

    /// The integration must not ship a hard-coded wake-up shortcut, and must not
    /// assert user-configurability as a verified fact: no retrieved
    /// documentation and no inspected settings established that here.
    #[test]
    fn start_voice_requires_a_user_supplied_trigger_and_claims_no_default() {
        let report = inspect(&WeChatInput);
        let finding = report
            .findings
            .iter()
            .find(|f| f.capability == Capability::StartVoice)
            .unwrap();
        assert!(
            finding.detail.contains("no trigger may be hard-coded"),
            "must refuse to hard-code a voice trigger"
        );
        assert!(
            finding
                .detail
                .contains("supplied by the user from their own client settings"),
            "must direct the caller to supply the configured trigger"
        );
        assert!(
            finding.detail.contains("neither a specific trigger")
                && finding
                    .detail
                    .contains("user-configurability is established"),
            "must not present user-configurability as an established fact"
        );
        assert_eq!(finding.support, Support::Unknown);
    }

    /// Every detail must carry the unreachable-source note so the gap is
    /// visible in the rendered report rather than hidden inside the module.
    #[test]
    fn details_record_the_missing_official_source() {
        let report = inspect(&WeChatInput);
        for finding in &report.findings {
            assert!(
                finding.detail.contains(SOURCE),
                "{} must cite the official source it could not retrieve",
                finding.capability.as_str()
            );
        }
    }

    #[test]
    fn wechat_mute_is_separated_from_playback_session_mute() {
        let report = inspect(&WeChatInput);
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
        assert!(playback.contains("separate Windows session CLI"));
        assert!(playback.contains("no WeType action integration"));
        assert!(!playback.contains("planned adapter"));
        assert!(app_mute.contains("WeType-native"));
    }

    #[test]
    fn details_do_not_claim_an_inspection_happened() {
        let report = inspect(&WeChatInput);
        for finding in &report.findings {
            let lower = finding.detail.to_lowercase();
            for phrase in [
                "we inspected",
                "the installed client was inspected",
                "confirmed absent",
                "no api exists",
            ] {
                assert!(
                    !lower.contains(phrase),
                    "{} implies a disproof or inspection that did not happen",
                    finding.capability.as_str()
                );
            }
        }
    }

    /// Runtime detail is machine-independent. A dated single-host snapshot is
    /// documentation, not detection: it must never appear in capability output,
    /// because the same text is emitted on every machine that runs this binary.
    #[test]
    fn runtime_detail_carries_no_machine_specific_host_facts() {
        let report = inspect(&WeChatInput);
        for finding in &report.findings {
            let lower = finding.detail.to_lowercase();
            for phrase in [
                "2.1.4.6",
                "is installed",
                "installed and running",
                "running with voice assets",
                "no listening tcp",
                "named pipe",
                "cli switch",
                "wetype_service",
                "wetype_server",
                "host check",
            ] {
                assert!(
                    !lower.contains(phrase),
                    "{} leaks a single-host snapshot fact into runtime output: {phrase}",
                    finding.capability.as_str()
                );
            }
        }
    }

    /// Every detail states plainly that no client was detected, so a reader of a
    /// runtime report cannot mistake the declaration for a host check.
    #[test]
    fn runtime_detail_states_no_client_was_detected() {
        let report = inspect(&WeChatInput);
        for finding in &report.findings {
            assert!(
                finding.detail.contains(NO_LOCAL_STATE),
                "{} must state that no installed client is detected here",
                finding.capability.as_str()
            );
        }
    }

    /// Unknown must be truthful: the detail may say why evidence is missing but
    /// must not assert the capability is absent, unsupported, or disproven.
    #[test]
    fn unknown_details_are_truthful_about_missing_evidence() {
        let report = inspect(&WeChatInput);
        for finding in &report.findings {
            assert_eq!(finding.support, Support::Unknown);
            let lower = finding.detail.to_lowercase();
            for phrase in [
                "is not supported",
                "does not support",
                "no control surface exists",
                "proves there is no",
                "confirms there is no",
            ] {
                assert!(
                    !lower.contains(phrase),
                    "{} asserts an absence that was never inspected",
                    finding.capability.as_str()
                );
            }
            assert!(
                lower.contains("undocumented, not disproven")
                    || lower.contains("undocumented is not absent")
                    || lower.contains("neither a specific trigger"),
                "{} must say the gap is missing evidence, not disproof",
                finding.capability.as_str()
            );
        }
    }
}
