//! WeChat Input (微信输入法 / WeType) voice integration probe.
//!
//! This probe is a static declaration. It does not inspect an installed client,
//! so no result here implies the local environment was examined.
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
//! Local read-only host inspection is recorded separately and deliberately does
//! not raise any grade: it confirmed the client is *installed and running*
//! (WeType 2.1.4.6) and ships voice assets, and it confirmed no listening TCP
//! port, no named pipe, and no CLI arguments. That establishes the absence of
//! an obvious local control surface. It does not establish that no control
//! exists — a private, undocumented interface remains possible — so
//! [`Support::Unsupported`] is still withheld. See
//! `docs/integrations/wechat-input.md` for the full evidence record.
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

/// Notes the one fact local inspection did establish, without letting it be
/// read as capability evidence.
const HOST_NOTE: &str = "read-only host check confirmed WeType 2.1.4.6 is installed and running with voice assets, and exposed no listening TCP port, named pipe, or CLI switch; a private undocumented interface is not excluded";

impl Probe for WeChatInput {
    fn integration(&self) -> &'static str {
        "wechat_input"
    }

    fn probe(&self, capability: Capability) -> Finding {
        let detail = match capability {
            Capability::Show => format!(
                "{SOURCE} was unreachable and no official-domain documentation described a \
                 programmatic show or focus API. {HOST_NOTE}. Undocumented, not disproven; \
                 {NOT_INSPECTED}."
            ),
            Capability::StartVoice => format!(
                "Voice entry is user-configurable in the client rather than fixed, so the \
                 trigger must be read from the user's own settings and never hard-coded. \
                 {SOURCE} was unreachable and no official source documented a default voice \
                 shortcut or an external start API. {HOST_NOTE}. Undocumented, not disproven; \
                 {NOT_INSPECTED}."
            ),
            Capability::EndVoice => format!(
                "{SOURCE} was unreachable and no official source documented how an in-progress \
                 voice session ends, or any external API for ending one. {HOST_NOTE}. \
                 Undocumented, not disproven; {NOT_INSPECTED}."
            ),
            Capability::AudioIdentity => format!(
                "Target audio identity requires live readback from the installed client. {SOURCE} \
                 was unreachable and no public API is documented; {HOST_NOTE}. {NOT_INSPECTED}."
            ),
            Capability::InputMute => format!(
                "Microphone input mute is handled by the device endpoint rather than by this \
                 integration. {SOURCE} was unreachable and a WeType-native input mute is \
                 undocumented, not disproven. {HOST_NOTE}; {NOT_INSPECTED}."
            ),
            Capability::AppMute => format!(
                "{SOURCE} was unreachable and no official source documented an output-mute API \
                 or a dedicated mute shortcut for this client. Undocumented is not absent; a \
                 WeType-native output mute remains unverified rather than unsupported. \
                 {HOST_NOTE}; {NOT_INSPECTED}."
            ),
            Capability::PlaybackSessionMute => format!(
                "Windows application-playback-session mute is a separate planned adapter, out \
                 of scope for this integration; {SOURCE} was unreachable and no official source \
                 described a WeType-native equivalent. Not conflated with any WeType-native mute \
                 control. {NOT_INSPECTED}."
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

    /// The local host check excluded an obvious control surface, but that is not
    /// a disproof of a private one. Unsupported claims a disproof, so it must
    /// not appear.
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

    /// The integration must not ship a hard-coded wake-up shortcut: the trigger
    /// is user-configured, so the detail must say to read it from the client.
    #[test]
    fn start_voice_requires_reading_the_configured_trigger() {
        let report = inspect(&WeChatInput);
        let finding = report
            .findings
            .iter()
            .find(|f| f.capability == Capability::StartVoice)
            .unwrap();
        assert!(
            finding.detail.contains("never hard-coded"),
            "must refuse to hard-code a voice trigger"
        );
        assert!(
            finding.detail.contains("user's own settings"),
            "must direct the caller to the configured shortcut"
        );
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
        assert!(playback.contains("separate planned adapter"));
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
}
