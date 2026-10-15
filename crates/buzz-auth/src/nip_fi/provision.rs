//! Issuer bindings written when a community is created.
//!
//! A deployment that provisions communities under one host suffix (for
//! example `<name>.communities.buzz.xyz`) binds each new community to a fixed
//! issuer allowlist at creation time. This is a write-time rule only: runtime
//! authorization reads the stored per-community rows, never this suffix.
//!
//! Hosts whose `https://<host>` equals a configured issuer's command audience
//! are *reserved*. Their community row may exist, but they never hold an
//! issuer binding, so a command audience can never become a community
//! assertion audience.

use std::collections::BTreeSet;

use buzz_core::tenant::normalize_host;

/// Environment variable carrying the provisioning binding JSON.
pub const PROVISION_BINDING_ENV: &str = "BUZZ_NIP_FI_PROVISION_BINDING";
/// Environment variable carrying the relay's issuer configs.
pub const ISSUERS_ENV: &str = "BUZZ_NIP_FI_ISSUERS";

/// Invalid provisioning binding configuration. The message never echoes
/// configured values.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("{0}")]
pub struct ProvisionBindingError(String);

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingJson {
    host_suffix: String,
    authorized_issuers: Vec<String>,
}

/// The fields of one `BUZZ_NIP_FI_ISSUERS` entry this module needs.
#[derive(serde::Deserialize)]
struct IssuerJson {
    issuer: String,
    #[serde(default)]
    command_audiences: Vec<String>,
}

/// Why a host cannot be bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindRefusal {
    /// The host's audience equals a configured command audience.
    ReservedHost,
    /// The issuer is not configured in `BUZZ_NIP_FI_ISSUERS`.
    UnknownIssuer,
}

/// Parsed provisioning binding: host suffix, issuers bound to new communities
/// under it, the configured issuer set, and the reserved command audiences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionBindingPolicy {
    host_suffix: String,
    authorized_issuers: Vec<String>,
    configured_issuers: BTreeSet<String>,
    command_audiences: BTreeSet<String>,
}

impl ProvisionBindingPolicy {
    /// Read the policy from the environment. `Ok(None)` when
    /// `BUZZ_NIP_FI_PROVISION_BINDING` is unset or empty: no community gets a
    /// binding at creation.
    pub fn from_env() -> Result<Option<Self>, ProvisionBindingError> {
        let binding = std::env::var(PROVISION_BINDING_ENV).unwrap_or_default();
        if binding.trim().is_empty() {
            return Ok(None);
        }
        let issuers = std::env::var(ISSUERS_ENV).unwrap_or_default();
        Self::parse(&binding, &issuers).map(Some)
    }

    /// Parse the binding JSON against the issuer config JSON.
    pub fn parse(binding_json: &str, issuers_json: &str) -> Result<Self, ProvisionBindingError> {
        let fail =
            |message: &str| ProvisionBindingError(format!("{PROVISION_BINDING_ENV}: {message}"));
        let binding: BindingJson = serde_json::from_str(binding_json)
            .map_err(|e| fail(&format!("invalid JSON ({:?})", e.classify())))?;
        if issuers_json.trim().is_empty() {
            return Err(fail(&format!("requires {ISSUERS_ENV}")));
        }
        let issuers: Vec<IssuerJson> = serde_json::from_str(issuers_json).map_err(|e| {
            fail(&format!(
                "{ISSUERS_ENV} is not valid JSON ({:?})",
                e.classify()
            ))
        })?;

        let host_suffix = binding.host_suffix;
        if host_suffix.is_empty()
            || normalize_host(&host_suffix) != host_suffix
            || host_suffix.contains(':')
            || host_suffix.split('.').any(str::is_empty)
        {
            return Err(fail(
                "host_suffix must be a normalized domain name without a port",
            ));
        }
        let configured_issuers: BTreeSet<String> =
            issuers.iter().map(|entry| entry.issuer.clone()).collect();
        let command_audiences: BTreeSet<String> = issuers
            .into_iter()
            .flat_map(|entry| entry.command_audiences)
            .collect();
        let mut authorized_issuers = binding.authorized_issuers;
        authorized_issuers.sort();
        authorized_issuers.dedup();
        if authorized_issuers.is_empty() {
            return Err(fail("authorized_issuers must not be empty"));
        }
        if authorized_issuers
            .iter()
            .any(|issuer| !configured_issuers.contains(issuer))
        {
            return Err(fail(&format!(
                "every authorized issuer must be configured in {ISSUERS_ENV}"
            )));
        }
        Ok(Self {
            host_suffix,
            authorized_issuers,
            configured_issuers,
            command_audiences,
        })
    }

    /// The configured host suffix.
    pub fn host_suffix(&self) -> &str {
        &self.host_suffix
    }

    /// Whether `host` is reserved: its `https://<host>` is a command audience.
    pub fn is_reserved(&self, host: &str) -> bool {
        self.command_audiences.contains(&format!("https://{host}"))
    }

    /// Hosts whose `https://<host>` is a configured command audience.
    pub fn reserved_hosts(&self) -> Vec<String> {
        self.command_audiences
            .iter()
            .filter_map(|aud| aud.strip_prefix("https://"))
            .map(str::to_owned)
            .collect()
    }

    /// Whether `host` is exactly one DNS label under the suffix, with no port.
    pub fn is_suffix_host(&self, host: &str) -> bool {
        host.strip_suffix(self.host_suffix.as_str())
            .and_then(|prefix| prefix.strip_suffix('.'))
            .is_some_and(|label| !label.is_empty() && !label.contains('.') && !label.contains(':'))
    }

    /// Issuers to bind to a newly created community at `host`: the authorized
    /// issuers for a non-reserved one-label suffix host, otherwise none.
    pub fn issuers_for_new_host(&self, host: &str) -> &[String] {
        if self.is_suffix_host(host) && !self.is_reserved(host) {
            &self.authorized_issuers
        } else {
            &[]
        }
    }

    /// Check an explicit operator bind of `issuer` to `host`.
    pub fn check_bind(&self, host: &str, issuer: &str) -> Result<(), BindRefusal> {
        if self.is_reserved(host) {
            return Err(BindRefusal::ReservedHost);
        }
        if !self.configured_issuers.contains(issuer) {
            return Err(BindRefusal::UnknownIssuer);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ISS: &str = "https://app.builderlab.xyz/api/goose";
    const OTHER: &str = "https://other.example";

    fn issuers() -> String {
        serde_json::json!([
            {"issuer": ISS, "command_audiences": ["https://communities.buzz.xyz"], "token_class": "nip-fi+jwt"},
            {"issuer": OTHER, "command_audiences": ["https://relay.other.example"]}
        ])
        .to_string()
    }

    fn policy() -> ProvisionBindingPolicy {
        ProvisionBindingPolicy::parse(
            &serde_json::json!({"host_suffix": "communities.buzz.xyz", "authorized_issuers": [ISS]})
                .to_string(),
            &issuers(),
        )
        .expect("valid policy")
    }

    #[test]
    fn provision_binding_binds_only_one_label_suffix_hosts() {
        let policy = policy();
        assert_eq!(
            policy.issuers_for_new_host("acme.communities.buzz.xyz"),
            [ISS]
        );
        for host in [
            "communities.buzz.xyz",
            "a.b.communities.buzz.xyz",
            "acme.communities.buzz.xyz:8443",
            "acmecommunities.buzz.xyz",
            ".communities.buzz.xyz",
            "acme.example",
        ] {
            assert!(policy.issuers_for_new_host(host).is_empty(), "{host}");
        }
    }

    #[test]
    fn provision_binding_reserves_command_audience_hosts() {
        let policy = policy();
        assert!(policy.is_reserved("communities.buzz.xyz"));
        assert!(policy.is_reserved("relay.other.example"));
        assert!(!policy.is_reserved("acme.communities.buzz.xyz"));
        assert_eq!(
            policy.reserved_hosts(),
            ["communities.buzz.xyz", "relay.other.example"]
        );
        assert_eq!(
            policy.check_bind("communities.buzz.xyz", ISS),
            Err(BindRefusal::ReservedHost)
        );
        assert_eq!(
            policy.check_bind("acme.communities.buzz.xyz", "https://unknown.example"),
            Err(BindRefusal::UnknownIssuer)
        );
        assert_eq!(
            policy.check_bind("acme.communities.buzz.xyz", OTHER),
            Ok(())
        );
    }

    #[test]
    fn provision_binding_rejects_invalid_config() {
        let binding = |suffix: &str, issuers: &[&str]| {
            serde_json::json!({"host_suffix": suffix, "authorized_issuers": issuers}).to_string()
        };
        for (binding, issuers) in [
            (binding("communities.buzz.xyz", &[]), issuers()),
            (
                binding("communities.buzz.xyz", &["https://unknown.example"]),
                issuers(),
            ),
            (binding("Communities.buzz.xyz", &[ISS]), issuers()),
            (binding("communities.buzz.xyz:443", &[ISS]), issuers()),
            (binding("communities..xyz", &[ISS]), issuers()),
            (binding("", &[ISS]), issuers()),
            (binding("communities.buzz.xyz", &[ISS]), String::new()),
            ("{\"host_suffix\": \"x\"}".to_string(), issuers()),
        ] {
            assert!(
                ProvisionBindingPolicy::parse(&binding, &issuers).is_err(),
                "{binding}"
            );
        }
    }
}
