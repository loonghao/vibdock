//! Capability probing and declarations for platform integrations.
//!
//! Integrations are described by [`Capability`] values and resolved at runtime
//! by [`Probe`] implementations. Only [`Support::Verified`] may be reported as
//! working; this keeps shortcut dispatch from being presented as confirmed
//! platform state.
//!
//! [`Support::Unverified`] means the capability is known to exist — it is
//! documented or otherwise declared — but no live state confirmed it. The
//! evidence for existence does not have to come from an inspection; a probe
//! that works purely from published documentation uses this level for what the
//! documentation describes.
//!
//! [`Support::Unknown`] is the level for capabilities with no evidence either
//! way. A probe reporting from documentation alone must use it: not finding a
//! documented API does not establish that the capability is absent. Only an
//! inspection that looked and found nothing may report
//! [`Support::Unsupported`].

use std::collections::BTreeMap;

/// A discrete control an integration may expose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Capability {
    Show,
    StartVoice,
    EndVoice,
    AudioIdentity,
    InputMute,
    AppMute,
    PlaybackSessionMute,
}

impl Capability {
    /// Stable identifier used in contracts, configuration, and reports.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Show => "show",
            Self::StartVoice => "start_voice",
            Self::EndVoice => "end_voice",
            Self::AudioIdentity => "audio_identity",
            Self::InputMute => "input_mute",
            Self::AppMute => "app_mute",
            Self::PlaybackSessionMute => "playback_session_mute",
        }
    }

    /// All capabilities, for enumeration in reports.
    pub const ALL: [Self; 7] = [
        Self::Show,
        Self::StartVoice,
        Self::EndVoice,
        Self::AudioIdentity,
        Self::InputMute,
        Self::AppMute,
        Self::PlaybackSessionMute,
    ];
}

/// How strongly a probe result can be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    /// Observed live on the installed client; safe to report as working.
    Verified,
    /// Known to exist — documented or otherwise declared — but no live
    /// confirmation. Existence evidence need not come from an inspection.
    /// Dispatch is not success.
    Unverified,
    /// Inspected and confirmed absent.
    #[allow(dead_code, reason = "available to adapters that do inspect")]
    Unsupported,
    /// Not inspected. Absence of documentation is not proof of absence.
    Unknown,
}

impl Support {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Unverified => "unverified",
            Self::Unsupported => "unsupported",
            Self::Unknown => "unknown",
        }
    }
}

/// One resolved capability, with evidence for the reported level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub capability: Capability,
    pub support: Support,
    pub detail: String,
}

/// Environment inspection used to resolve a capability.
pub trait Probe {
    /// Integration name, e.g. `codex`.
    fn integration(&self) -> &'static str;
    fn probe(&self, capability: Capability) -> Finding;
}

/// Aggregated result for one integration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub integration: String,
    pub findings: Vec<Finding>,
}

impl Report {
    /// Every capability resolved as [`Support::Verified`].
    pub fn verified(&self) -> Vec<Capability> {
        self.findings
            .iter()
            .filter(|f| f.support == Support::Verified)
            .map(|f| f.capability)
            .collect()
    }

    /// True when at least one finding is not verified.
    ///
    /// Named for the condition it reports, not for the grade it is most often
    /// confused with: a report holding a mix of verified and unresolved entries
    /// returns true here as well.
    #[allow(dead_code, reason = "part of the report contract for consumers")]
    pub fn has_unverified(&self) -> bool {
        self.findings.iter().any(|f| f.support != Support::Verified)
    }

    /// True when at least one finding was never inspected.
    pub fn has_unknown(&self) -> bool {
        self.findings.iter().any(|f| f.support == Support::Unknown)
    }

    /// Render as `name=level` pairs, stable and easy to diff.
    pub fn summary(&self) -> BTreeMap<String, &'static str> {
        self.findings
            .iter()
            .map(|f| (f.capability.as_str().to_string(), f.support.as_str()))
            .collect()
    }
}

/// Run every capability through a probe.
pub fn inspect(probe: &dyn Probe) -> Report {
    Report {
        integration: probe.integration().to_string(),
        findings: Capability::ALL.iter().map(|c| probe.probe(*c)).collect(),
    }
}

/// Print capability reports for every registered integration.
pub fn report_all() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let probes: [&dyn Probe; 1] = [&crate::integrations::codex::Codex];
    for probe in probes {
        let report = inspect(probe);
        println!("{}", report.integration);
        for finding in &report.findings {
            println!(
                "  {:<22} {:<12} {}",
                finding.capability.as_str(),
                finding.support.as_str(),
                finding.detail
            );
        }
        let verified: Vec<_> = report
            .verified()
            .iter()
            .map(|c| c.as_str())
            .collect::<Vec<_>>();
        if verified.is_empty() {
            println!(
                "  verified: none ({} capabilities not verified; dispatch is not success)",
                report.findings.len()
            );
            println!(
                "  note: no entry is verified; these declarations are documentation-derived, not an inspection of the installed client"
            );
        } else {
            println!("  verified: {}", verified.join(", "));
        }
        if report.has_unknown() {
            println!(
                "  note: unknown entries were never inspected; they are not claims of absence"
            );
        }
        let summary: Vec<_> = report
            .summary()
            .iter()
            .map(|(name, level)| format!("{name}={level}"))
            .collect();
        println!("  summary: {}", summary.join(" "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Stub;
    impl Probe for Stub {
        fn integration(&self) -> &'static str {
            "stub"
        }
        fn probe(&self, capability: Capability) -> Finding {
            let support = match capability {
                Capability::Show => Support::Verified,
                Capability::AudioIdentity => Support::Unverified,
                Capability::PlaybackSessionMute => Support::Unknown,
                _ => Support::Unsupported,
            };
            Finding {
                capability,
                support,
                detail: "stub".to_string(),
            }
        }
    }

    #[test]
    fn only_verified_capabilities_are_reported_as_working() {
        let report = inspect(&Stub);
        assert_eq!(report.verified(), vec![Capability::Show]);
        assert!(report.has_unverified());
    }

    #[test]
    fn summary_keys_are_capability_ids() {
        let report = inspect(&Stub);
        let summary = report.summary();
        assert_eq!(summary["show"], "verified");
        assert_eq!(summary["start_voice"], "unsupported");
        assert_eq!(summary["playback_session_mute"], "unknown");
        assert_eq!(summary.len(), Capability::ALL.len());
    }

    #[test]
    fn unknown_is_distinct_from_unsupported() {
        let report = inspect(&Stub);
        let unknown = report
            .findings
            .iter()
            .filter(|f| f.support == Support::Unknown)
            .count();
        assert_eq!(unknown, 1);
        assert!(report.has_unknown());
        assert_ne!(Support::Unknown.as_str(), Support::Unsupported.as_str());
    }

    #[test]
    fn capability_ids_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for capability in Capability::ALL {
            assert!(seen.insert(capability.as_str()));
        }
    }

    /// A report that mixes grades has both a non-empty verified list and
    /// unverified entries. The report printer must key its "nothing is
    /// verified" note off the verified list, not off `has_unverified`, or a
    /// mixed report contradicts itself.
    #[test]
    fn mixed_report_has_both_verified_and_unverified() {
        struct Mixed;
        impl Probe for Mixed {
            fn integration(&self) -> &'static str {
                "mixed"
            }
            fn probe(&self, capability: Capability) -> Finding {
                let support = if capability == Capability::Show {
                    Support::Verified
                } else {
                    Support::Unknown
                };
                Finding {
                    capability,
                    support,
                    detail: "mixed".to_string(),
                }
            }
        }
        let report = inspect(&Mixed);
        assert_eq!(report.verified(), vec![Capability::Show]);
        assert!(report.has_unverified());
        assert!(report.has_unknown());
    }
}
