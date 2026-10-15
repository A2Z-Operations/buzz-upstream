//! NIP-FI community issuer bindings: backfill and explicit operator edits.
//!
//! Reads `BUZZ_NIP_FI_PROVISION_BINDING` and `BUZZ_NIP_FI_ISSUERS` with the
//! relay's own parser, so the CLI binds exactly what relay provisioning would.

use anyhow::{Context, Result};
use buzz_auth::{BindRefusal, ProvisionBindingPolicy};
use clap::Subcommand;
use serde_json::json;

use crate::communities::normalize_host_authority;

/// NIP-FI binding actions.
#[derive(Debug, Subcommand)]
pub enum NipFiCommand {
    /// Bind every unbound one-label suffix community to the configured
    /// issuers, then report how many remain unbound (must be 0 before enforce).
    Backfill,
    /// Bind one issuer to one community.
    Bind {
        /// Exact community hostname.
        #[arg(long)]
        host: String,
        /// Issuer configured in `BUZZ_NIP_FI_ISSUERS`.
        #[arg(long)]
        issuer: String,
    },
    /// Remove one issuer from one community.
    Unbind {
        /// Exact community hostname.
        #[arg(long)]
        host: String,
        /// Bound issuer to remove.
        #[arg(long)]
        issuer: String,
    },
}

/// Run a NIP-FI binding action.
pub async fn run(command: NipFiCommand) -> Result<i32> {
    let policy =
        ProvisionBindingPolicy::from_env()?.context("BUZZ_NIP_FI_PROVISION_BINDING is required")?;
    match command {
        NipFiCommand::Backfill => backfill(&policy).await,
        NipFiCommand::Bind { host, issuer } => bind(&policy, &host, &issuer).await,
        NipFiCommand::Unbind { host, issuer } => unbind(&host, &issuer).await,
    }
}

async fn backfill(policy: &ProvisionBindingPolicy) -> Result<i32> {
    let db = crate::connect_db().await?;
    let mut bound = 0u64;
    for community in db
        .list_unbound_nip_fi_communities(policy.host_suffix())
        .await?
    {
        bound += db
            .bind_nip_fi_issuers(community.id, policy.issuers_for_new_host(&community.host))
            .await?;
    }
    let unbound: Vec<String> = db
        .list_unbound_nip_fi_communities(policy.host_suffix())
        .await?
        .into_iter()
        .map(|community| community.host)
        .filter(|host| !policy.issuers_for_new_host(host).is_empty())
        .collect();
    println!(
        "{}",
        json!({"rows_bound": bound, "unbound": unbound.len(), "unbound_hosts": unbound})
    );
    Ok(i32::from(!unbound.is_empty()))
}

async fn bind(policy: &ProvisionBindingPolicy, host: &str, issuer: &str) -> Result<i32> {
    let host = normalize_host_authority(host).map_err(anyhow::Error::msg)?;
    match policy.check_bind(&host, issuer) {
        Ok(()) => {}
        Err(BindRefusal::ReservedHost) => {
            anyhow::bail!("host is reserved: it is a NIP-FI command audience")
        }
        Err(BindRefusal::UnknownIssuer) => {
            anyhow::bail!("issuer is not configured in BUZZ_NIP_FI_ISSUERS")
        }
    }
    let db = crate::connect_db().await?;
    let community = db
        .lookup_community_by_host_for_management(&host)
        .await?
        .context("no community has this host")?;
    let inserted = db
        .bind_nip_fi_issuers(community.id, &[issuer.to_string()])
        .await?;
    println!(
        "{}",
        json!({"host": community.host, "issuer": issuer, "bound": inserted > 0})
    );
    Ok(0)
}

async fn unbind(host: &str, issuer: &str) -> Result<i32> {
    let host = normalize_host_authority(host).map_err(anyhow::Error::msg)?;
    let db = crate::connect_db().await?;
    let community = db
        .lookup_community_by_host_for_management(&host)
        .await?
        .context("no community has this host")?;
    let removed = db.unbind_nip_fi_issuer(community.id, issuer).await?;
    println!(
        "{}",
        json!({"host": community.host, "issuer": issuer, "removed": removed})
    );
    Ok(0)
}
