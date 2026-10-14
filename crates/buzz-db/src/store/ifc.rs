//! Canonical policy cuts and atomic fenced publication.

use std::collections::BTreeMap;

use buzz_core::ifc::{IfcPublicationFence, IfcPublicationTarget};
use buzz_core::{CommunityId, StoredEvent};
use chrono::{DateTime, Utc};
use nostr::Event;
use sqlx::Row;
use thiserror::Error;
use uuid::Uuid;

use crate::event::{
    insert_canvas_head_checked_tx, insert_event_with_thread_metadata_tx, ChannelHeadPrecondition,
    ChannelHeadWriteStatus, EventQuery, ThreadMetadataParams,
};
use crate::replaceable::{ParameterizedReplacePrecondition, ParameterizedReplaceStatus};
use crate::{Db, DbError};

/// Largest canonical active-reader set included in one proof.
pub const IFC_MAX_POLICY_MEMBERS: i64 = 50_000;
/// Largest number of raw resources included in one proof.
pub const IFC_MAX_RESOURCES: i64 = 500;

/// Canonical channel policy captured under its serialization lock.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IfcPolicySnapshot {
    /// Channel identity.
    pub channel_id: Uuid,
    /// Monotone policy generation.
    pub generation: i64,
    /// Canonical channel type.
    pub channel_type: String,
    /// Canonical visibility.
    pub visibility: String,
    /// Effective retained-history authorization.
    pub history_access: String,
    /// Whether publication is archived.
    pub archived: bool,
    /// Sorted active member public keys.
    pub members: Vec<String>,
}

/// One serialized policy cut and the raw events read under it.
#[derive(Debug)]
pub struct IfcReadCut {
    /// Canonical policy.
    pub policy: IfcPolicySnapshot,
    /// Exact signed resources selected by the supported query.
    pub resources: Vec<StoredEvent>,
}

/// Result of one fenced publication transaction.
#[derive(Debug)]
pub enum IfcPublishOutcome {
    /// This transaction inserted and owns post-commit dispatch.
    Inserted(Box<StoredEvent>),
    /// The exact event was accepted by an earlier committed transaction.
    HistoricDuplicate,
    /// Canvas optimistic-concurrency validation rejected the candidate.
    CanvasConflict(ChannelHeadWriteStatus),
    /// Owner-memory LWW ordering rejected the candidate.
    MemoryConflict(ParameterizedReplaceStatus),
}

/// Result of committing a fenced ephemeral acceptance receipt.
#[derive(Debug, Eq, PartialEq)]
pub enum IfcEphemeralOutcome {
    /// This transaction committed acceptance and owns the first dispatch.
    Accepted,
    /// The exact event was accepted by an earlier committed transaction.
    HistoricDuplicate,
}

/// Stable fenced-read and publication failures.
#[derive(Debug, Error)]
pub enum IfcError {
    /// A supplied policy generation is no longer current.
    #[error("ifc-policy-changed")]
    PolicyChanged,
    /// A new publication fence has expired or exceeds its lifetime bound.
    #[error("ifc-expired")]
    Expired,
    /// Canonical policy is missing, inactive, or unsupported.
    #[error("ifc-invalid")]
    Invalid,
    /// The authenticated principal lacks ordinary channel access.
    #[error("access denied")]
    AccessDenied,
    /// Database failure.
    #[error(transparent)]
    Database(#[from] DbError),
}

async fn lock_policies(
    tx: &mut crate::AdmittedTx,
    channels: impl IntoIterator<Item = Uuid>,
) -> Result<(), IfcError> {
    let mut channels = channels.into_iter().collect::<Vec<_>>();
    channels.sort_unstable();
    channels.dedup();
    for channel in channels {
        sqlx::query("SELECT pg_advisory_xact_lock(channel_policy_lock_key($1, $2))")
            .bind(tx.community().as_uuid())
            .bind(channel)
            .execute(tx.conn())
            .await
            .map_err(DbError::from)?;
    }
    Ok(())
}

async fn load_policy(
    tx: &mut crate::AdmittedTx,
    channel_id: Uuid,
) -> Result<IfcPolicySnapshot, IfcError> {
    let row = sqlx::query(
        "SELECT p.generation, p.deleted, c.channel_type::text, c.visibility::text, \
                c.history_access, c.archived_at IS NOT NULL AS archived \
         FROM channel_policy_generations p \
         JOIN channels c ON c.community_id = p.community_id AND c.id = p.channel_id \
         WHERE p.community_id = $1 AND p.channel_id = $2 AND c.deleted_at IS NULL",
    )
    .bind(tx.community().as_uuid())
    .bind(channel_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(DbError::from)?
    .ok_or(IfcError::Invalid)?;
    if row.try_get::<bool, _>("deleted").map_err(DbError::from)? {
        return Err(IfcError::Invalid);
    }
    let history_access: String = row.try_get("history_access").map_err(DbError::from)?;
    if history_access != "all_current_members" {
        return Err(IfcError::Invalid);
    }
    let member_rows = sqlx::query(
        "SELECT encode(pubkey, 'hex') AS pubkey \
         FROM channel_members \
         WHERE community_id = $1 AND channel_id = $2 AND removed_at IS NULL \
         ORDER BY pubkey LIMIT $3",
    )
    .bind(tx.community().as_uuid())
    .bind(channel_id)
    .bind(IFC_MAX_POLICY_MEMBERS + 1)
    .fetch_all(tx.conn())
    .await
    .map_err(DbError::from)?;
    if member_rows.len() as i64 > IFC_MAX_POLICY_MEMBERS {
        return Err(IfcError::Invalid);
    }
    let members = member_rows
        .into_iter()
        .map(|member| member.try_get::<String, _>("pubkey").map_err(DbError::from))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(IfcPolicySnapshot {
        channel_id,
        generation: row.try_get("generation").map_err(DbError::from)?,
        channel_type: row.try_get("channel_type").map_err(DbError::from)?,
        visibility: row.try_get("visibility").map_err(DbError::from)?,
        history_access,
        archived: row.try_get("archived").map_err(DbError::from)?,
        members,
    })
}

fn has_ordinary_read(policy: &IfcPolicySnapshot, principal_hex: &str) -> bool {
    policy.visibility == "open"
        || policy
            .members
            .binary_search_by(|member| member.as_str().cmp(principal_hex))
            .is_ok()
}

async fn lock_event_receipt(
    tx: &mut crate::AdmittedTx,
    event_id: &[u8; 32],
) -> Result<(), IfcError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!(
            "buzz_ifc_event:{}:{}",
            tx.community(),
            hex::encode(event_id)
        ))
        .execute(tx.conn())
        .await
        .map_err(DbError::from)?;
    Ok(())
}

async fn validate_known_memory_owner(
    tx: &mut crate::AdmittedTx,
    event: &Event,
    owner_pubkey: &str,
) -> Result<(), IfcError> {
    let owner = hex::decode(owner_pubkey).map_err(|_| IfcError::Invalid)?;
    let registered: Option<(Option<Vec<u8>>,)> = sqlx::query_as(
        "SELECT agent_owner_pubkey FROM users \
         WHERE community_id=$1 AND pubkey=$2 FOR SHARE",
    )
    .bind(tx.community().as_uuid())
    .bind(event.pubkey.as_bytes())
    .fetch_optional(tx.conn())
    .await
    .map_err(DbError::from)?;
    if registered
        .and_then(|(registered,)| registered)
        .is_some_and(|registered| registered != owner)
    {
        return Err(IfcError::AccessDenied);
    }
    Ok(())
}

impl Db {
    /// Capture a canonical policy and bounded raw history under one lock cut.
    pub async fn ifc_read_cut(
        &self,
        community_id: CommunityId,
        channel_id: Uuid,
        principal_hex: &str,
        expected_generation: Option<i64>,
        query: &EventQuery,
    ) -> Result<IfcReadCut, IfcError> {
        if query.community_id != community_id
            || query.channel_id != Some(channel_id)
            || query.limit.unwrap_or(IFC_MAX_RESOURCES) > IFC_MAX_RESOURCES
        {
            return Err(IfcError::Invalid);
        }
        let mut tx = crate::begin_community_write_transaction(
            &self.pool,
            community_id,
            crate::observability::WriterOperation::Authorization,
        )
        .await?;
        sqlx::query("SET LOCAL statement_timeout = '9s'")
            .execute(tx.conn())
            .await
            .map_err(DbError::from)?;
        sqlx::query("SET LOCAL lock_timeout = '9s'")
            .execute(tx.conn())
            .await
            .map_err(DbError::from)?;
        lock_policies(&mut tx, [channel_id]).await?;
        let policy = load_policy(&mut tx, channel_id).await?;
        if !has_ordinary_read(&policy, principal_hex) {
            return Err(IfcError::AccessDenied);
        }
        if expected_generation.is_some_and(|generation| generation != policy.generation) {
            return Err(IfcError::PolicyChanged);
        }
        let resources = crate::event::query_events_on(tx.conn(), query).await?;
        tx.rollback().await?;
        Ok(IfcReadCut { policy, resources })
    }

    /// Return whether an exact fenced event has a committed acceptance receipt.
    pub async fn ifc_event_was_accepted(
        &self,
        community_id: CommunityId,
        event_id: &[u8; 32],
    ) -> Result<bool, DbError> {
        let row: Option<(bool,)> = sqlx::query_as(
            "SELECT TRUE FROM ifc_event_acceptances WHERE community_id = $1 AND event_id = $2",
        )
        .bind(community_id.as_uuid())
        .bind(event_id.as_slice())
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.is_some())
    }

    /// Atomically validate canonical sources and accept one ephemeral telemetry event.
    ///
    /// The event itself remains ephemeral. Its durable receipt is the commit point;
    /// only the caller receiving [`IfcEphemeralOutcome::Accepted`] may dispatch it.
    pub async fn accept_ifc_ephemeral(
        &self,
        community_id: CommunityId,
        event: &Event,
        fence: &IfcPublicationFence,
        principal_hex: &str,
    ) -> Result<IfcEphemeralOutcome, IfcError> {
        if fence.community_id != *community_id.as_uuid()
            || u32::from(event.kind.as_u16()) != buzz_core::ifc::IFC_OWNER_TELEMETRY_KIND
            || !matches!(&fence.target, IfcPublicationTarget::OwnerTelemetry { .. })
        {
            return Err(IfcError::Invalid);
        }
        let mut tx = crate::begin_community_event_write_transaction(
            &self.pool,
            community_id,
            crate::observability::WriterOperation::EventWrite,
        )
        .await?;
        lock_event_receipt(&mut tx, event.id.as_bytes()).await?;
        let accepted: Option<(bool,)> = sqlx::query_as(
            "SELECT TRUE FROM ifc_event_acceptances WHERE community_id = $1 AND event_id = $2",
        )
        .bind(community_id.as_uuid())
        .bind(event.id.as_bytes().as_slice())
        .fetch_optional(tx.conn())
        .await
        .map_err(DbError::from)?;
        if accepted.is_some() {
            tx.rollback().await?;
            return Ok(IfcEphemeralOutcome::HistoricDuplicate);
        }

        let now: i64 = sqlx::query_scalar("SELECT EXTRACT(EPOCH FROM clock_timestamp())::BIGINT")
            .fetch_one(tx.conn())
            .await
            .map_err(DbError::from)?;
        if fence.expires_at <= now || fence.expires_at > now + buzz_core::ifc::IFC_MAX_TTL_SECONDS {
            return Err(IfcError::Expired);
        }

        let required = fence
            .sources
            .iter()
            .map(|source| (source.channel_id, source.generation))
            .collect::<BTreeMap<_, _>>();
        lock_policies(&mut tx, required.keys().copied()).await?;
        for (channel_id, generation) in required {
            let policy = load_policy(&mut tx, channel_id).await?;
            if policy.generation != generation {
                return Err(IfcError::PolicyChanged);
            }
            if policy.archived || !has_ordinary_read(&policy, principal_hex) {
                return Err(IfcError::AccessDenied);
            }
        }

        let commit_check_now: i64 =
            sqlx::query_scalar("SELECT EXTRACT(EPOCH FROM clock_timestamp())::BIGINT")
                .fetch_one(tx.conn())
                .await
                .map_err(DbError::from)?;
        if fence.expires_at <= commit_check_now {
            return Err(IfcError::Expired);
        }
        let expires_at =
            DateTime::<Utc>::from_timestamp(fence.expires_at, 0).ok_or(IfcError::Invalid)?;
        sqlx::query(
            "INSERT INTO ifc_event_acceptances \
             (community_id, event_id, event_kind, fence_expires_at) VALUES ($1, $2, $3, $4)",
        )
        .bind(community_id.as_uuid())
        .bind(event.id.as_bytes().as_slice())
        .bind(i32::from(event.kind.as_u16()))
        .bind(expires_at)
        .execute(tx.conn())
        .await
        .map_err(DbError::from)?;
        match tx.commit().await {
            Ok(()) => Ok(IfcEphemeralOutcome::Accepted),
            Err(DbError::Sqlx(sqlx::Error::Database(error)))
                if error.constraint() == Some("ifc_acceptance_unexpired") =>
            {
                Err(IfcError::Expired)
            }
            Err(error) => Err(IfcError::Database(error)),
        }
    }

    /// Atomically validate canonical policies, insert an event, and record acceptance.
    pub async fn publish_ifc_event(
        &self,
        community_id: CommunityId,
        event: &Event,
        fence: &IfcPublicationFence,
        principal_hex: &str,
        thread_meta: Option<ThreadMetadataParams<'_>>,
        canvas_precondition: Option<ChannelHeadPrecondition<'_>>,
    ) -> Result<IfcPublishOutcome, IfcError> {
        if fence.community_id != *community_id.as_uuid() {
            return Err(IfcError::Invalid);
        }
        if matches!(&fence.target, IfcPublicationTarget::OwnerTelemetry { .. }) {
            return Err(IfcError::Invalid);
        }
        let mut tx = crate::begin_community_event_write_transaction(
            &self.pool,
            community_id,
            crate::observability::WriterOperation::EventWrite,
        )
        .await?;
        tx.set_push_enqueue(self.push_enqueue.clone());
        lock_event_receipt(&mut tx, event.id.as_bytes()).await?;
        let accepted: Option<(bool,)> = sqlx::query_as(
            "SELECT TRUE FROM ifc_event_acceptances WHERE community_id = $1 AND event_id = $2",
        )
        .bind(community_id.as_uuid())
        .bind(event.id.as_bytes().as_slice())
        .fetch_optional(tx.conn())
        .await
        .map_err(DbError::from)?;
        if accepted.is_some() {
            tx.rollback().await?;
            return Ok(IfcPublishOutcome::HistoricDuplicate);
        }

        let now: i64 = sqlx::query_scalar("SELECT EXTRACT(EPOCH FROM clock_timestamp())::BIGINT")
            .fetch_one(tx.conn())
            .await
            .map_err(DbError::from)?;
        if fence.expires_at <= now || fence.expires_at > now + buzz_core::ifc::IFC_MAX_TTL_SECONDS {
            return Err(IfcError::Expired);
        }

        // Match each established event-write order before taking policy locks.
        // The normal insert path reacquires the same transaction locks.
        match &fence.target {
            IfcPublicationTarget::Channel(destination) => {
                crate::event::acquire_canvas_event_write_lock_if_needed(
                    &mut tx,
                    event,
                    Some(destination.channel_id),
                )
                .await?;
                if super::event_follow_up::ttl_refresh_channel(
                    Some(destination.channel_id),
                    i32::from(event.kind.as_u16()),
                )
                .is_some()
                {
                    super::event_follow_up::refresh_channel_ttl(
                        tx.conn(),
                        community_id,
                        destination.channel_id,
                    )
                    .await?;
                }
            }
            IfcPublicationTarget::OwnerMemory {
                owner_pubkey,
                d_tag,
            } => {
                super::replaceable::acquire_parameterized_event_write_lock(&mut tx, event, d_tag)
                    .await?;
                validate_known_memory_owner(&mut tx, event, owner_pubkey).await?;
            }
            IfcPublicationTarget::OwnerTelemetry { .. } => return Err(IfcError::Invalid),
        }

        let mut required = BTreeMap::new();
        for source in &fence.sources {
            required.insert(source.channel_id, source.generation);
        }
        if let IfcPublicationTarget::Channel(destination) = &fence.target {
            match required.insert(destination.channel_id, destination.generation) {
                Some(source_generation) if source_generation != destination.generation => {
                    return Err(IfcError::PolicyChanged);
                }
                _ => {}
            }
        }
        lock_policies(&mut tx, required.keys().copied()).await?;
        for (channel_id, generation) in required {
            let policy = load_policy(&mut tx, channel_id).await?;
            if policy.generation != generation {
                return Err(IfcError::PolicyChanged);
            }
            if policy.archived || !has_ordinary_read(&policy, principal_hex) {
                return Err(IfcError::AccessDenied);
            }
        }

        let commit_check_now: i64 =
            sqlx::query_scalar("SELECT EXTRACT(EPOCH FROM clock_timestamp())::BIGINT")
                .fetch_one(tx.conn())
                .await
                .map_err(DbError::from)?;
        if fence.expires_at <= commit_check_now {
            return Err(IfcError::Expired);
        }

        let outcome = match &fence.target {
            IfcPublicationTarget::Channel(destination) => {
                if let Some(precondition) = canvas_precondition {
                    let (stored, status) = insert_canvas_head_checked_tx(
                        &mut tx,
                        event,
                        destination.channel_id,
                        precondition,
                    )
                    .await?;
                    if status != ChannelHeadWriteStatus::Inserted {
                        tx.rollback().await?;
                        return Ok(IfcPublishOutcome::CanvasConflict(status));
                    }
                    IfcPublishOutcome::Inserted(Box::new(stored))
                } else {
                    let (stored, inserted) = insert_event_with_thread_metadata_tx(
                        &mut tx,
                        event,
                        Some(destination.channel_id),
                        thread_meta,
                    )
                    .await?;
                    if !inserted {
                        return Err(IfcError::Invalid);
                    }
                    crate::insert_mentions_in_transaction(
                        &mut tx,
                        event,
                        Some(destination.channel_id),
                    )
                    .await?;
                    IfcPublishOutcome::Inserted(Box::new(stored))
                }
            }
            IfcPublicationTarget::OwnerMemory { d_tag, .. } => {
                if thread_meta.is_some() || canvas_precondition.is_some() {
                    return Err(IfcError::Invalid);
                }
                let replacement = self
                    .replace_parameterized_event_in_transaction(
                        &mut tx,
                        event,
                        d_tag,
                        None,
                        ParameterizedReplacePrecondition::Unconditional,
                    )
                    .await?;
                match replacement.status {
                    ParameterizedReplaceStatus::Inserted => {
                        IfcPublishOutcome::Inserted(Box::new(replacement.event))
                    }
                    ParameterizedReplaceStatus::Superseded
                    | ParameterizedReplaceStatus::RevisionMissing
                    | ParameterizedReplaceStatus::RevisionMismatch
                    | ParameterizedReplaceStatus::ReplayOnlyMiss => {
                        tx.rollback().await?;
                        return Ok(IfcPublishOutcome::MemoryConflict(replacement.status));
                    }
                    ParameterizedReplaceStatus::Duplicate => return Err(IfcError::Invalid),
                }
            }
            IfcPublicationTarget::OwnerTelemetry { .. } => return Err(IfcError::Invalid),
        };

        let expires_at =
            DateTime::<Utc>::from_timestamp(fence.expires_at, 0).ok_or(IfcError::Invalid)?;
        sqlx::query(
            "INSERT INTO ifc_event_acceptances \
             (community_id, event_id, event_kind, fence_expires_at) VALUES ($1, $2, $3, $4)",
        )
        .bind(community_id.as_uuid())
        .bind(event.id.as_bytes().as_slice())
        .bind(i32::from(event.kind.as_u16()))
        .bind(expires_at)
        .execute(tx.conn())
        .await
        .map_err(DbError::from)?;
        match tx.commit().await {
            Ok(()) => {}
            Err(DbError::Sqlx(sqlx::Error::Database(error)))
                if error.constraint() == Some("ifc_acceptance_unexpired") =>
            {
                return Err(IfcError::Expired);
            }
            Err(error) => return Err(IfcError::Database(error)),
        }
        Ok(outcome)
    }

    /// Verify that the required generation catalog and database triggers exist.
    pub async fn verify_ifc_policy_catalog(&self) -> Result<(), DbError> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT tgname FROM pg_trigger \
             WHERE NOT tgisinternal AND tgenabled = 'O' AND tgname = ANY($1::text[]) ORDER BY tgname",
        )
        .bind([
            "trg_channel_members_ifc_policy_delete",
            "trg_channel_members_ifc_policy_identity",
            "trg_channel_members_ifc_policy_insert",
            "trg_channel_members_ifc_policy_update",
            "trg_channels_ifc_policy_delete",
            "trg_channels_ifc_policy_identity",
            "trg_channels_ifc_policy_insert",
            "trg_channels_ifc_policy_update",
        ])
        .fetch_all(&self.pool)
        .await?;
        let catalog_ready: bool = sqlx::query_scalar(
            "SELECT to_regclass('channel_policy_generations') IS NOT NULL \
                 AND to_regclass('ifc_event_acceptances') IS NOT NULL \
                 AND EXISTS (SELECT 1 FROM information_schema.columns \
                             WHERE table_schema='public' AND table_name='channels' \
                               AND column_name='history_access') \
                 AND to_regprocedure('channel_policy_lock_key(uuid,uuid)') IS NOT NULL \
                 AND to_regprocedure('initialize_channel_policy_generation()') IS NOT NULL \
                 AND to_regprocedure('advance_channel_policy_for_channel_update()') IS NOT NULL \
                 AND to_regprocedure('advance_channel_policy_for_member_update()') IS NOT NULL \
                 AND to_regprocedure('guard_channel_policy_generation_row()') IS NOT NULL \
                 AND to_regprocedure('guard_ifc_acceptance_row()') IS NOT NULL \
                 AND to_regprocedure('enforce_ifc_acceptance_expiry()') IS NOT NULL \
                 AND to_regprocedure('prune_ifc_acceptances_after_insert()') IS NOT NULL \
                 AND EXISTS (SELECT 1 FROM pg_trigger \
                             WHERE tgname='trg_ifc_acceptance_unexpired' AND tgenabled='O') \
                 AND EXISTS (SELECT 1 FROM pg_trigger \
                             WHERE tgname='trg_ifc_acceptance_prune' AND tgenabled='O') \
                 AND EXISTS (SELECT 1 FROM pg_trigger \
                             WHERE tgname='trg_channel_policy_generation_guard' AND tgenabled='O') \
                 AND EXISTS (SELECT 1 FROM pg_trigger \
                             WHERE tgname='trg_ifc_acceptance_guard' AND tgenabled='O')",
        )
        .fetch_one(&self.pool)
        .await?;
        if rows.len() != 8 || !catalog_ready {
            return Err(DbError::InvalidData(
                "IFC policy catalog is incomplete".to_owned(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod postgres_tests {
    use super::*;
    use crate::{migration, DbConfig};
    use buzz_core::ifc::parse_publication_fence;
    use buzz_core::kind::{KIND_AGENT_ENGRAM, KIND_AGENT_OBSERVER_FRAME, KIND_STREAM_MESSAGE};
    use buzz_core::observer::{
        encrypt_observer_payload, OBSERVER_AGENT_TAG, OBSERVER_FRAME_TAG, OBSERVER_FRAME_TELEMETRY,
    };
    use nostr::{EventBuilder, Keys, Kind, Tag, Timestamp};
    use sqlx::PgPool;

    async fn connect() -> Db {
        let database_url = std::env::var("BUZZ_TEST_DATABASE_URL")
            .expect("Postgres wrapper must provide an isolated database URL");
        let migration_schema = std::env::var("BUZZ_TEST_SCHEMA_MODE").as_deref() == Ok("migration");
        let db = Db::new(&DbConfig {
            database_url,
            ..DbConfig::default()
        })
        .await
        .expect("connect isolated database");
        if migration_schema {
            migration::run_migrations(db.pool())
                .await
                .expect("apply migrations");
        }
        db.verify_ifc_policy_catalog()
            .await
            .expect("verify IFC policy catalog");
        db
    }

    async fn fixture(pool: &PgPool) -> (CommunityId, Uuid, Keys) {
        let community_uuid = Uuid::new_v4();
        sqlx::query("INSERT INTO communities (id, host) VALUES ($1, $2)")
            .bind(community_uuid)
            .bind(format!("ifc-{}.example", community_uuid.simple()))
            .execute(pool)
            .await
            .expect("insert community");
        let community = CommunityId::from_uuid(community_uuid);
        let keys = Keys::generate();
        let channel = crate::channel::create_channel(
            pool,
            community,
            "ifc-test",
            crate::channel::ChannelType::Stream,
            crate::channel::ChannelVisibility::Private,
            None,
            &keys.public_key().to_bytes(),
            None,
        )
        .await
        .expect("create channel");
        (community, channel.id, keys)
    }

    async fn generation(pool: &PgPool, community: CommunityId, channel: Uuid) -> i64 {
        sqlx::query_scalar(
            "SELECT generation FROM channel_policy_generations WHERE community_id=$1 AND channel_id=$2",
        )
        .bind(community.as_uuid())
        .bind(channel)
        .fetch_one(pool)
        .await
        .expect("read generation")
    }

    fn fenced_event(
        keys: &Keys,
        community: CommunityId,
        channel: Uuid,
        generation: i64,
        expires_at: i64,
        content: &str,
    ) -> Event {
        let channel_text = channel.to_string();
        let community_text = community.to_string();
        let generation_text = generation.to_string();
        let expiry_text = expires_at.to_string();
        EventBuilder::new(Kind::Custom(KIND_STREAM_MESSAGE as u16), content)
            .tags([
                Tag::parse(["h", channel_text.as_str()]).expect("h tag"),
                Tag::parse(["ifc", "1", community_text.as_str(), expiry_text.as_str()])
                    .expect("ifc tag"),
                Tag::parse([
                    "ifc-source",
                    channel_text.as_str(),
                    generation_text.as_str(),
                ])
                .expect("source tag"),
                Tag::parse([
                    "ifc-destination",
                    channel_text.as_str(),
                    generation_text.as_str(),
                ])
                .expect("destination tag"),
            ])
            .sign_with_keys(keys)
            .expect("sign event")
    }

    fn fenced_event_between(
        keys: &Keys,
        community: CommunityId,
        source: (Uuid, i64),
        destination: (Uuid, i64),
        content: &str,
    ) -> Event {
        let source_channel = source.0.to_string();
        let source_generation = source.1.to_string();
        let destination_channel = destination.0.to_string();
        let destination_generation = destination.1.to_string();
        let community_text = community.to_string();
        let expiry = (Utc::now().timestamp() + 120).to_string();
        EventBuilder::new(Kind::Custom(KIND_STREAM_MESSAGE as u16), content)
            .tags([
                Tag::parse(["h", destination_channel.as_str()]).expect("h tag"),
                Tag::parse(["ifc", "1", community_text.as_str(), expiry.as_str()])
                    .expect("ifc tag"),
                Tag::parse([
                    "ifc-source",
                    source_channel.as_str(),
                    source_generation.as_str(),
                ])
                .expect("source tag"),
                Tag::parse([
                    "ifc-destination",
                    destination_channel.as_str(),
                    destination_generation.as_str(),
                ])
                .expect("destination tag"),
            ])
            .sign_with_keys(keys)
            .expect("sign event")
    }

    fn fenced_memory_event(
        keys: &Keys,
        owner: &Keys,
        community: CommunityId,
        source: (Uuid, i64),
        d_tag: &str,
        timing: (u64, i64),
        content: &str,
    ) -> Event {
        let owner_hex = owner.public_key().to_hex();
        let source_channel = source.0.to_string();
        let source_generation = source.1.to_string();
        let community_text = community.to_string();
        let expiry = timing.1.to_string();
        EventBuilder::new(Kind::Custom(KIND_AGENT_ENGRAM as u16), content)
            .tags([
                Tag::parse(["d", d_tag]).expect("d tag"),
                Tag::parse(["p", owner_hex.as_str()]).expect("p tag"),
                Tag::parse(["ifc", "1", community_text.as_str(), expiry.as_str()])
                    .expect("ifc tag"),
                Tag::parse([
                    "ifc-source",
                    source_channel.as_str(),
                    source_generation.as_str(),
                ])
                .expect("source tag"),
                Tag::parse(["ifc-owner-memory", owner_hex.as_str()]).expect("owner-memory tag"),
            ])
            .custom_created_at(Timestamp::from(timing.0))
            .sign_with_keys(keys)
            .expect("sign owner memory")
    }

    fn fenced_telemetry_event(
        agent: &Keys,
        owner: &Keys,
        community: CommunityId,
        source: (Uuid, i64),
        expires_at: i64,
        label: &str,
    ) -> Event {
        let owner_hex = owner.public_key().to_hex();
        let agent_hex = agent.public_key().to_hex();
        let source_channel = source.0.to_string();
        let source_generation = source.1.to_string();
        let community_text = community.to_string();
        let expiry = expires_at.to_string();
        let ciphertext = encrypt_observer_payload(
            agent,
            &owner.public_key(),
            &serde_json::json!({"type": "tool_result", "label": label}),
        )
        .expect("encrypt telemetry");
        EventBuilder::new(Kind::Custom(KIND_AGENT_OBSERVER_FRAME as u16), ciphertext)
            .tags([
                Tag::parse(["p", owner_hex.as_str()]).expect("p tag"),
                Tag::parse([OBSERVER_AGENT_TAG, agent_hex.as_str()]).expect("agent tag"),
                Tag::parse([OBSERVER_FRAME_TAG, OBSERVER_FRAME_TELEMETRY]).expect("frame tag"),
                Tag::parse(["ifc", "1", community_text.as_str(), expiry.as_str()])
                    .expect("ifc tag"),
                Tag::parse([
                    "ifc-source",
                    source_channel.as_str(),
                    source_generation.as_str(),
                ])
                .expect("source tag"),
                Tag::parse(["ifc-owner-telemetry", owner_hex.as_str()])
                    .expect("owner-telemetry tag"),
            ])
            .sign_with_keys(agent)
            .expect("sign telemetry")
    }

    #[tokio::test]
    #[ignore = "requires isolated Postgres"]
    async fn policy_generation_tracks_semantic_changes_and_leave_rejoin_aba() {
        let db = connect().await;
        let (community, channel, keys) = fixture(db.pool()).await;
        assert_eq!(generation(db.pool(), community, channel).await, 1);

        sqlx::query(
            "UPDATE channels SET name=name, topic='new topic' WHERE community_id=$1 AND id=$2",
        )
        .bind(community.as_uuid())
        .bind(channel)
        .execute(db.pool())
        .await
        .expect("metadata update");
        sqlx::query("UPDATE channel_members SET role=role WHERE community_id=$1 AND channel_id=$2")
            .bind(community.as_uuid())
            .bind(channel)
            .execute(db.pool())
            .await
            .expect("no-op member update");
        assert_eq!(generation(db.pool(), community, channel).await, 1);

        sqlx::query("UPDATE channel_members SET removed_at=NOW() WHERE community_id=$1 AND channel_id=$2 AND pubkey=$3")
            .bind(community.as_uuid()).bind(channel).bind(keys.public_key().to_bytes().as_slice()).execute(db.pool()).await.expect("leave");
        assert_eq!(generation(db.pool(), community, channel).await, 2);
        sqlx::query("UPDATE channel_members SET removed_at=NULL WHERE community_id=$1 AND channel_id=$2 AND pubkey=$3")
            .bind(community.as_uuid()).bind(channel).bind(keys.public_key().to_bytes().as_slice()).execute(db.pool()).await.expect("rejoin");
        assert_eq!(generation(db.pool(), community, channel).await, 3);
        sqlx::query("UPDATE channels SET visibility='open' WHERE community_id=$1 AND id=$2")
            .bind(community.as_uuid())
            .bind(channel)
            .execute(db.pool())
            .await
            .expect("visibility update");
        assert_eq!(generation(db.pool(), community, channel).await, 4);
        assert!(
            sqlx::query("UPDATE channels SET id=$3 WHERE community_id=$1 AND id=$2",)
                .bind(community.as_uuid())
                .bind(channel)
                .bind(Uuid::new_v4())
                .execute(db.pool())
                .await
                .is_err()
        );
        sqlx::query(
            "UPDATE channel_members SET role='admin' \
             WHERE community_id=$1 AND channel_id=$2 AND pubkey=$3",
        )
        .bind(community.as_uuid())
        .bind(channel)
        .bind(keys.public_key().to_bytes().as_slice())
        .execute(db.pool())
        .await
        .expect("change active authority");
        assert_eq!(generation(db.pool(), community, channel).await, 5);
        sqlx::query("UPDATE channels SET archived_at=NOW() WHERE community_id=$1 AND id=$2")
            .bind(community.as_uuid())
            .bind(channel)
            .execute(db.pool())
            .await
            .expect("archive channel");
        assert_eq!(generation(db.pool(), community, channel).await, 6);
        sqlx::query("UPDATE channels SET archived_at=NULL WHERE community_id=$1 AND id=$2")
            .bind(community.as_uuid())
            .bind(channel)
            .execute(db.pool())
            .await
            .expect("unarchive channel");
        assert_eq!(generation(db.pool(), community, channel).await, 7);
        assert!(sqlx::query(
            "UPDATE channels SET history_access='future_policy' \
                 WHERE community_id=$1 AND id=$2",
        )
        .bind(community.as_uuid())
        .bind(channel)
        .execute(db.pool())
        .await
        .is_err());
        assert_eq!(generation(db.pool(), community, channel).await, 7);
        sqlx::query("DELETE FROM channels WHERE community_id=$1 AND id=$2")
            .bind(community.as_uuid())
            .bind(channel)
            .execute(db.pool())
            .await
            .expect("hard delete channel");
        let tombstone: (i64, bool) = sqlx::query_as(
            "SELECT generation, deleted FROM channel_policy_generations \
             WHERE community_id=$1 AND channel_id=$2",
        )
        .bind(community.as_uuid())
        .bind(channel)
        .fetch_one(db.pool())
        .await
        .expect("policy tombstone");
        assert!(tombstone.0 > 7 && tombstone.1);
        assert!(sqlx::query(
            "INSERT INTO channels (id, community_id, name, created_by) VALUES ($1,$2,$3,$4)",
        )
        .bind(channel)
        .bind(community.as_uuid())
        .bind("reused-channel")
        .bind(keys.public_key().to_bytes().as_slice())
        .execute(db.pool())
        .await
        .is_err());
        assert!(sqlx::query(
            "UPDATE channel_policy_generations SET generation=1 \
             WHERE community_id=$1 AND channel_id=$2",
        )
        .bind(community.as_uuid())
        .bind(channel)
        .execute(db.pool())
        .await
        .is_err());
    }

    #[tokio::test]
    #[ignore = "requires isolated Postgres"]
    async fn receipt_retention_prunes_old_entries_only_through_trigger() {
        let db = connect().await;
        let (community, _, _) = fixture(db.pool()).await;
        let old_event_id = [7_u8; 32];
        let mut cleanup = crate::begin_community_write_transaction(
            db.pool(),
            community,
            crate::observability::WriterOperation::Maintenance,
        )
        .await
        .expect("begin receipt cleanup probe");
        sqlx::query(
            "INSERT INTO ifc_event_acceptances \
             (community_id, event_id, event_kind, fence_expires_at, accepted_at) \
             VALUES ($1, $2, 9, NOW() + INTERVAL '1 minute', NOW() - INTERVAL '25 hours')",
        )
        .bind(community.as_uuid())
        .bind(old_event_id.as_slice())
        .execute(cleanup.conn())
        .await
        .expect("insert old receipt");
        cleanup
            .commit()
            .await
            .expect("commit receipt cleanup probe");
        assert!(!db
            .ifc_event_was_accepted(community, &old_event_id)
            .await
            .expect("query pruned receipt"));
    }

    #[tokio::test]
    #[ignore = "requires isolated Postgres"]
    async fn exact_retry_keeps_historic_acceptance_after_policy_change() {
        let db = connect().await;
        let (community, channel, keys) = fixture(db.pool()).await;
        let event = fenced_event(
            &keys,
            community,
            channel,
            1,
            Utc::now().timestamp() + 120,
            "first",
        );
        let fence = parse_publication_fence(&event)
            .expect("parse")
            .expect("fence");
        let first = db
            .publish_ifc_event(
                community,
                &event,
                &fence,
                &keys.public_key().to_hex(),
                None,
                None,
            )
            .await;
        assert!(
            matches!(first, Ok(IfcPublishOutcome::Inserted(_))),
            "{first:?}"
        );
        sqlx::query("UPDATE channels SET visibility='open' WHERE community_id=$1 AND id=$2")
            .bind(community.as_uuid())
            .bind(channel)
            .execute(db.pool())
            .await
            .expect("policy change");
        assert!(matches!(
            db.publish_ifc_event(
                community,
                &event,
                &fence,
                &keys.public_key().to_hex(),
                None,
                None
            )
            .await,
            Ok(IfcPublishOutcome::HistoricDuplicate)
        ));
        assert!(sqlx::query(
            "DELETE FROM ifc_event_acceptances WHERE community_id=$1 AND event_id=$2",
        )
        .bind(community.as_uuid())
        .bind(event.id.as_bytes().as_slice())
        .execute(db.pool())
        .await
        .is_err());
        let stale = fenced_event(
            &keys,
            community,
            channel,
            1,
            Utc::now().timestamp() + 120,
            "stale",
        );
        let stale_fence = parse_publication_fence(&stale)
            .expect("parse")
            .expect("fence");
        assert!(matches!(
            db.publish_ifc_event(
                community,
                &stale,
                &stale_fence,
                &keys.public_key().to_hex(),
                None,
                None
            )
            .await,
            Err(IfcError::PolicyChanged)
        ));
        let expired = fenced_event(
            &keys,
            community,
            channel,
            2,
            Utc::now().timestamp() - 1,
            "expired",
        );
        let expired_fence = parse_publication_fence(&expired)
            .expect("parse")
            .expect("fence");
        assert!(matches!(
            db.publish_ifc_event(
                community,
                &expired,
                &expired_fence,
                &keys.public_key().to_hex(),
                None,
                None
            )
            .await,
            Err(IfcError::Expired)
        ));
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM events WHERE community_id=$1")
            .bind(community.as_uuid())
            .fetch_one(db.pool())
            .await
            .expect("count events");
        assert_eq!(count, 1);
    }

    #[tokio::test]
    #[ignore = "requires isolated Postgres"]
    async fn concurrent_exact_duplicates_have_one_insert_owner() {
        let db = connect().await;
        let (community, channel, keys) = fixture(db.pool()).await;
        let event = fenced_event(
            &keys,
            community,
            channel,
            1,
            Utc::now().timestamp() + 120,
            "race",
        );
        let fence = parse_publication_fence(&event)
            .expect("parse")
            .expect("fence");
        let principal = keys.public_key().to_hex();
        let (left, right) = tokio::join!(
            db.publish_ifc_event(community, &event, &fence, &principal, None, None),
            db.publish_ifc_event(community, &event, &fence, &principal, None, None),
        );
        let outcomes = [left, right];
        let inserted = outcomes
            .iter()
            .filter(|outcome| matches!(outcome, Ok(IfcPublishOutcome::Inserted(_))))
            .count();
        assert_eq!(inserted, 1, "{outcomes:?}");
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM ifc_event_acceptances WHERE community_id=$1")
                .bind(community.as_uuid())
                .fetch_one(db.pool())
                .await
                .expect("count receipts");
        assert_eq!(count, 1);
    }

    #[tokio::test]
    #[ignore = "requires isolated Postgres"]
    async fn ephemeral_telemetry_receipt_is_atomic_without_event_storage() {
        let db = connect().await;
        let (community, channel, agent) = fixture(db.pool()).await;
        let owner = Keys::generate();
        let event = fenced_telemetry_event(
            &agent,
            &owner,
            community,
            (channel, 1),
            Utc::now().timestamp() + 120,
            "accepted",
        );
        let fence = parse_publication_fence(&event)
            .expect("parse")
            .expect("fence");
        assert_eq!(
            db.accept_ifc_ephemeral(community, &event, &fence, &agent.public_key().to_hex(),)
                .await
                .expect("accept telemetry"),
            IfcEphemeralOutcome::Accepted
        );
        let stored: i64 =
            sqlx::query_scalar("SELECT count(*) FROM events WHERE community_id=$1 AND id=$2")
                .bind(community.as_uuid())
                .bind(event.id.as_bytes().as_slice())
                .fetch_one(db.pool())
                .await
                .expect("count stored telemetry");
        assert_eq!(stored, 0);

        sqlx::query("UPDATE channels SET visibility='open' WHERE community_id=$1 AND id=$2")
            .bind(community.as_uuid())
            .bind(channel)
            .execute(db.pool())
            .await
            .expect("advance policy");
        assert_eq!(
            db.accept_ifc_ephemeral(community, &event, &fence, &agent.public_key().to_hex(),)
                .await
                .expect("historic telemetry retry"),
            IfcEphemeralOutcome::HistoricDuplicate
        );

        let stale = fenced_telemetry_event(
            &agent,
            &owner,
            community,
            (channel, 1),
            Utc::now().timestamp() + 120,
            "stale",
        );
        let stale_fence = parse_publication_fence(&stale)
            .expect("parse stale")
            .expect("stale fence");
        assert!(matches!(
            db.accept_ifc_ephemeral(
                community,
                &stale,
                &stale_fence,
                &agent.public_key().to_hex(),
            )
            .await,
            Err(IfcError::PolicyChanged)
        ));
        let expired = fenced_telemetry_event(
            &agent,
            &owner,
            community,
            (channel, 2),
            Utc::now().timestamp() - 1,
            "expired",
        );
        let expired_fence = parse_publication_fence(&expired)
            .expect("parse expired")
            .expect("expired fence");
        assert!(matches!(
            db.accept_ifc_ephemeral(
                community,
                &expired,
                &expired_fence,
                &agent.public_key().to_hex(),
            )
            .await,
            Err(IfcError::Expired)
        ));
    }

    #[tokio::test]
    #[ignore = "requires isolated Postgres"]
    async fn concurrent_ephemeral_duplicates_have_one_dispatch_owner() {
        let db = connect().await;
        let (community, channel, agent) = fixture(db.pool()).await;
        let owner = Keys::generate();
        let event = fenced_telemetry_event(
            &agent,
            &owner,
            community,
            (channel, 1),
            Utc::now().timestamp() + 120,
            "concurrent",
        );
        let fence = parse_publication_fence(&event)
            .expect("parse")
            .expect("fence");
        let principal = agent.public_key().to_hex();
        let (left, right) = tokio::join!(
            db.accept_ifc_ephemeral(community, &event, &fence, &principal),
            db.accept_ifc_ephemeral(community, &event, &fence, &principal),
        );
        let outcomes = [left, right];
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, Ok(IfcEphemeralOutcome::Accepted)))
                .count(),
            1,
            "{outcomes:?}"
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, Ok(IfcEphemeralOutcome::HistoricDuplicate)))
                .count(),
            1,
            "{outcomes:?}"
        );
    }

    #[tokio::test]
    #[ignore = "requires isolated Postgres"]
    async fn queued_telemetry_serializes_after_source_membership_change() {
        let db = connect().await;
        let (community, channel, agent) = fixture(db.pool()).await;
        let owner = Keys::generate();
        let event = fenced_telemetry_event(
            &agent,
            &owner,
            community,
            (channel, 1),
            Utc::now().timestamp() + 120,
            "queued",
        );
        let fence = parse_publication_fence(&event)
            .expect("parse")
            .expect("fence");
        let mut mutation = db.pool().begin().await.expect("begin membership change");
        sqlx::query(
            "UPDATE channel_members SET removed_at=NOW() \
             WHERE community_id=$1 AND channel_id=$2 AND pubkey=$3",
        )
        .bind(community.as_uuid())
        .bind(channel)
        .bind(agent.public_key().to_bytes().as_slice())
        .execute(&mut *mutation)
        .await
        .expect("hold membership change");
        let queued_db = db.clone();
        let principal = agent.public_key().to_hex();
        let queued = tokio::spawn(async move {
            queued_db
                .accept_ifc_ephemeral(community, &event, &fence, &principal)
                .await
        });
        tokio::task::yield_now().await;
        mutation.commit().await.expect("commit membership change");
        assert!(matches!(
            queued.await.expect("queued task"),
            Err(IfcError::PolicyChanged)
        ));
        let receipts: i64 =
            sqlx::query_scalar("SELECT count(*) FROM ifc_event_acceptances WHERE community_id=$1")
                .bind(community.as_uuid())
                .fetch_one(db.pool())
                .await
                .expect("count receipts");
        assert_eq!(receipts, 0);
    }

    #[tokio::test]
    #[ignore = "requires isolated Postgres"]
    async fn current_member_reads_legacy_history_at_current_generation() {
        let db = connect().await;
        let (community, channel, keys) = fixture(db.pool()).await;
        let channel_text = channel.to_string();
        let legacy = EventBuilder::new(Kind::Custom(KIND_STREAM_MESSAGE as u16), "legacy")
            .tags([Tag::parse(["h", channel_text.as_str()]).expect("h tag")])
            .sign_with_keys(&keys)
            .expect("sign legacy event");
        db.insert_event_with_thread_metadata(community, &legacy, Some(channel), None)
            .await
            .expect("insert legacy event");
        let added = Keys::generate();
        sqlx::query(
            "INSERT INTO channel_members (community_id, channel_id, pubkey, role, invited_by) \
             VALUES ($1, $2, $3, 'member', $4)",
        )
        .bind(community.as_uuid())
        .bind(channel)
        .bind(added.public_key().to_bytes().as_slice())
        .bind(keys.public_key().to_bytes().as_slice())
        .execute(db.pool())
        .await
        .expect("add current reader");
        let mut query = EventQuery::for_community(community);
        query.channel_id = Some(channel);
        query.kinds = Some(vec![KIND_STREAM_MESSAGE as i32]);
        query.limit = Some(10);
        let cut = db
            .ifc_read_cut(
                community,
                channel,
                &added.public_key().to_hex(),
                Some(2),
                &query,
            )
            .await
            .expect("current full-history grant");
        assert_eq!(cut.policy.generation, 2);
        assert_eq!(cut.resources.len(), 1);
        assert_eq!(cut.resources[0].event.id, legacy.id);
        let outsider = Keys::generate();
        assert!(matches!(
            db.ifc_read_cut(
                community,
                channel,
                &outsider.public_key().to_hex(),
                Some(2),
                &query
            )
            .await,
            Err(IfcError::AccessDenied)
        ));
    }

    #[tokio::test]
    #[ignore = "requires isolated Postgres"]
    async fn source_and_destination_generations_are_checked_independently() {
        let db = connect().await;
        let (community, source, keys) = fixture(db.pool()).await;
        let destination = crate::channel::create_channel(
            db.pool(),
            community,
            "ifc-destination",
            crate::channel::ChannelType::Stream,
            crate::channel::ChannelVisibility::Private,
            None,
            &keys.public_key().to_bytes(),
            None,
        )
        .await
        .expect("create destination")
        .id;
        sqlx::query("UPDATE channels SET visibility='open' WHERE community_id=$1 AND id=$2")
            .bind(community.as_uuid())
            .bind(source)
            .execute(db.pool())
            .await
            .expect("advance source");
        let stale_source = fenced_event_between(
            &keys,
            community,
            (source, 1),
            (destination, 1),
            "stale source",
        );
        let fence = parse_publication_fence(&stale_source)
            .expect("parse")
            .expect("fence");
        assert!(matches!(
            db.publish_ifc_event(
                community,
                &stale_source,
                &fence,
                &keys.public_key().to_hex(),
                None,
                None
            )
            .await,
            Err(IfcError::PolicyChanged)
        ));
        sqlx::query("UPDATE channels SET visibility='open' WHERE community_id=$1 AND id=$2")
            .bind(community.as_uuid())
            .bind(destination)
            .execute(db.pool())
            .await
            .expect("advance destination");
        let stale_destination = fenced_event_between(
            &keys,
            community,
            (source, 2),
            (destination, 1),
            "stale destination",
        );
        let fence = parse_publication_fence(&stale_destination)
            .expect("parse")
            .expect("fence");
        assert!(matches!(
            db.publish_ifc_event(
                community,
                &stale_destination,
                &fence,
                &keys.public_key().to_hex(),
                None,
                None
            )
            .await,
            Err(IfcError::PolicyChanged)
        ));
    }

    #[tokio::test]
    #[ignore = "requires isolated Postgres"]
    async fn fenced_canvas_preserves_head_preconditions() {
        let db = connect().await;
        let (community, channel, keys) = fixture(db.pool()).await;
        let channel_text = channel.to_string();
        let community_text = community.to_string();
        let expiry = (Utc::now().timestamp() + 120).to_string();
        let tags = || {
            [
                Tag::parse(["h", channel_text.as_str()]).expect("h tag"),
                Tag::parse(["ifc", "1", community_text.as_str(), expiry.as_str()])
                    .expect("ifc tag"),
                Tag::parse(["ifc-source", channel_text.as_str(), "1"]).expect("source tag"),
                Tag::parse(["ifc-destination", channel_text.as_str(), "1"])
                    .expect("destination tag"),
            ]
        };
        let now = Utc::now().timestamp() as u64;
        let first = EventBuilder::new(Kind::Custom(buzz_core::kind::KIND_CANVAS as u16), "first")
            .tags(tags())
            .custom_created_at(Timestamp::from(now))
            .sign_with_keys(&keys)
            .expect("sign first canvas");
        let first_fence = parse_publication_fence(&first)
            .expect("parse")
            .expect("fence");
        assert!(matches!(
            db.publish_ifc_event(
                community,
                &first,
                &first_fence,
                &keys.public_key().to_hex(),
                None,
                Some(ChannelHeadPrecondition::ExpectNoHead)
            )
            .await,
            Ok(IfcPublishOutcome::Inserted(_))
        ));
        let second = EventBuilder::new(Kind::Custom(buzz_core::kind::KIND_CANVAS as u16), "second")
            .tags(tags())
            .custom_created_at(Timestamp::from(now + 1))
            .sign_with_keys(&keys)
            .expect("sign second canvas");
        let second_fence = parse_publication_fence(&second)
            .expect("parse")
            .expect("fence");
        assert!(matches!(
            db.publish_ifc_event(
                community,
                &second,
                &second_fence,
                &keys.public_key().to_hex(),
                None,
                Some(ChannelHeadPrecondition::ExpectNoHead)
            )
            .await,
            Ok(IfcPublishOutcome::CanvasConflict(
                ChannelHeadWriteStatus::RevisionMismatch
            ))
        ));
        assert!(matches!(
            db.publish_ifc_event(
                community,
                &second,
                &second_fence,
                &keys.public_key().to_hex(),
                None,
                Some(ChannelHeadPrecondition::ExpectedHead(first.id.as_bytes()))
            )
            .await,
            Ok(IfcPublishOutcome::Inserted(_))
        ));
    }

    #[tokio::test]
    #[ignore = "requires isolated Postgres"]
    async fn owner_memory_uses_atomic_lww_receipts_and_fixed_owner() {
        let db = connect().await;
        let (community, source, agent) = fixture(db.pool()).await;
        let owner = Keys::generate();
        let wrong_owner = Keys::generate();
        for keys in [&agent, &owner, &wrong_owner] {
            db.ensure_user(community, keys.public_key().as_bytes())
                .await
                .expect("ensure owner user");
        }
        assert!(db
            .set_agent_owner(
                community,
                agent.public_key().as_bytes(),
                owner.public_key().as_bytes(),
            )
            .await
            .expect("set fixed owner"));
        let d_tag = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let now = Utc::now().timestamp() as u64;
        let first = fenced_memory_event(
            &agent,
            &owner,
            community,
            (source, 1),
            d_tag,
            (now, Utc::now().timestamp() + 120),
            "first ciphertext",
        );
        let first_fence = parse_publication_fence(&first)
            .expect("parse first")
            .expect("first fence");
        assert!(matches!(
            db.publish_ifc_event(
                community,
                &first,
                &first_fence,
                &agent.public_key().to_hex(),
                None,
                None
            )
            .await,
            Ok(IfcPublishOutcome::Inserted(_))
        ));

        let second = fenced_memory_event(
            &agent,
            &owner,
            community,
            (source, 1),
            d_tag,
            (now + 1, Utc::now().timestamp() + 120),
            "second ciphertext",
        );
        let second_fence = parse_publication_fence(&second)
            .expect("parse second")
            .expect("second fence");
        assert!(matches!(
            db.publish_ifc_event(
                community,
                &second,
                &second_fence,
                &agent.public_key().to_hex(),
                None,
                None
            )
            .await,
            Ok(IfcPublishOutcome::Inserted(_))
        ));

        let stale = fenced_memory_event(
            &agent,
            &owner,
            community,
            (source, 1),
            d_tag,
            (now - 1, Utc::now().timestamp() + 120),
            "stale ciphertext",
        );
        let stale_fence = parse_publication_fence(&stale)
            .expect("parse stale")
            .expect("stale fence");
        assert!(matches!(
            db.publish_ifc_event(
                community,
                &stale,
                &stale_fence,
                &agent.public_key().to_hex(),
                None,
                None
            )
            .await,
            Ok(IfcPublishOutcome::MemoryConflict(
                ParameterizedReplaceStatus::Superseded
            ))
        ));
        assert!(!db
            .ifc_event_was_accepted(community, stale.id.as_bytes())
            .await
            .expect("stale receipt"));

        sqlx::query("UPDATE channels SET visibility='open' WHERE community_id=$1 AND id=$2")
            .bind(community.as_uuid())
            .bind(source)
            .execute(db.pool())
            .await
            .expect("advance source generation");
        assert!(matches!(
            db.publish_ifc_event(
                community,
                &first,
                &first_fence,
                &agent.public_key().to_hex(),
                None,
                None
            )
            .await,
            Ok(IfcPublishOutcome::HistoricDuplicate)
        ));

        let wrong = fenced_memory_event(
            &agent,
            &wrong_owner,
            community,
            (source, 2),
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            (now + 2, Utc::now().timestamp() + 120),
            "wrong owner ciphertext",
        );
        let wrong_fence = parse_publication_fence(&wrong)
            .expect("parse wrong owner")
            .expect("wrong owner fence");
        assert!(matches!(
            db.publish_ifc_event(
                community,
                &wrong,
                &wrong_fence,
                &agent.public_key().to_hex(),
                None,
                None
            )
            .await,
            Err(IfcError::AccessDenied)
        ));

        let expires_at = Utc::now().timestamp() + 2;
        let expiring = fenced_memory_event(
            &agent,
            &owner,
            community,
            (source, 2),
            "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            (now + 3, expires_at),
            "expiring ciphertext",
        );
        let expiring_fence = parse_publication_fence(&expiring)
            .expect("parse expiring")
            .expect("expiring fence");
        assert!(matches!(
            db.publish_ifc_event(
                community,
                &expiring,
                &expiring_fence,
                &agent.public_key().to_hex(),
                None,
                None
            )
            .await,
            Ok(IfcPublishOutcome::Inserted(_))
        ));
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        assert!(matches!(
            db.publish_ifc_event(
                community,
                &expiring,
                &expiring_fence,
                &agent.public_key().to_hex(),
                None,
                None
            )
            .await,
            Ok(IfcPublishOutcome::HistoricDuplicate)
        ));

        let active: Vec<(Vec<u8>,)> = sqlx::query_as(
            "SELECT id FROM events WHERE community_id=$1 AND kind=$2 AND pubkey=$3 \
             AND d_tag=$4 AND deleted_at IS NULL",
        )
        .bind(community.as_uuid())
        .bind(KIND_AGENT_ENGRAM as i32)
        .bind(agent.public_key().as_bytes())
        .bind(d_tag)
        .fetch_all(db.pool())
        .await
        .expect("read active memory head");
        assert_eq!(active, vec![(second.id.as_bytes().to_vec(),)]);
    }

    #[tokio::test]
    #[ignore = "requires isolated Postgres"]
    async fn membership_commit_serializes_read_and_fenced_write() {
        let db = connect().await;
        let (community, channel, keys) = fixture(db.pool()).await;
        let event = fenced_event(
            &keys,
            community,
            channel,
            1,
            Utc::now().timestamp() + 120,
            "racing write",
        );
        let fence = parse_publication_fence(&event)
            .expect("parse")
            .expect("fence");
        let mut mutation = db.pool().begin().await.expect("begin membership change");
        sqlx::query(
            "UPDATE channel_members SET removed_at=NOW() \
             WHERE community_id=$1 AND channel_id=$2 AND pubkey=$3",
        )
        .bind(community.as_uuid())
        .bind(channel)
        .bind(keys.public_key().to_bytes().as_slice())
        .execute(&mut *mutation)
        .await
        .expect("hold canonical membership change");

        let mut query = EventQuery::for_community(community);
        query.channel_id = Some(channel);
        query.kinds = Some(vec![KIND_STREAM_MESSAGE as i32]);
        query.limit = Some(10);
        let read_db = db.clone();
        let reader = keys.public_key().to_hex();
        let read = tokio::spawn(async move {
            read_db
                .ifc_read_cut(community, channel, &reader, None, &query)
                .await
        });
        let write_db = db.clone();
        let writer = keys.public_key().to_hex();
        let write = tokio::spawn(async move {
            write_db
                .publish_ifc_event(community, &event, &fence, &writer, None, None)
                .await
        });
        tokio::task::yield_now().await;
        mutation.commit().await.expect("commit membership change");

        assert!(matches!(
            read.await.expect("read task"),
            Err(IfcError::AccessDenied)
        ));
        assert!(matches!(
            write.await.expect("write task"),
            Err(IfcError::PolicyChanged)
        ));
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM events WHERE community_id=$1")
            .bind(community.as_uuid())
            .fetch_one(db.pool())
            .await
            .expect("count events");
        assert_eq!(count, 0);
    }
}
