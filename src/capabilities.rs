//! Capability probing and declarations for platform integrations.
//!
//! Integrations are described by [`Capability`] values and resolved at runtime
//! by [`Probe`] implementations. A probe returning [`Support::Unverified`] means
//! the environment was inspected but no live state was confirmed; only
//! [`Support::Verified`] may be reported as working. This distinction keeps
//! shortcut dispatch from being presented as confirmed platform state.

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
        }
    }

    /// All capabilities, for enumeration in reports.
    pub const ALL: [Self; 6] = [
        Self::Show,
        Self::StartVoice,
        Self::EndVoice,
        Self::AudioIdentity,
        Self::InputMute,
        Self::AppMute,
    ];
}

/// How strongly a probe result can be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    /// Observed live on the installed client; safe to report as working.
    Verified,
    /// Environment inspected, no live confirmation. Dispatch is not success.
    Unverified,
    /// Inspected and confirmed absent.
    Unsupported,
}

impl Support {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Unverified => "unverified",
            Self::Unsupported => "unsupported",
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
    pub fn has_unverified(&self) -> bool {
        self.findings.iter().any(|f| f.support != Support::Verified)
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
                "  {:<15} {:<12} {}",
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
        } else {
            println!("  verified: {}", verified.join(", "));
        }
        if report.has_unverified() {
            println!("  note: unverified entries require installed-client evidence");
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
        assert_eq!(summary.len(), Capability::ALL.len());
    }

    #[test]
    fn capability_ids_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for capability in Capability::ALL {
            assert!(seen.insert(capability.as_str()));
        }
    }
}
