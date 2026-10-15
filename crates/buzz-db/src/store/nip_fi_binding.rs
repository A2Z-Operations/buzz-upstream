//! Per-community NIP-FI issuer bindings (`community_nip_fi_issuers`).
//!
//! Operator-plane persistence only: creation paths, the backfill, and the
//! operator lookup route write and read these rows. The assertion audience of
//! a bound community is `https://<communities.host>`.

use buzz_core::CommunityId;
use buzz_datastore_tracing::datastore_span;
use sqlx::{Postgres, Row, Transaction};

use crate::{CommunityRecord, Db, Result};

/// A community's stored NIP-FI binding, as returned to the operator route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NipFiBindingRecord {
    /// Canonical host stored on the community row.
    pub host: String,
    /// True when the community is not archived and has no deletion in progress.
    pub active: bool,
    /// Bound issuers in byte order. Empty means no binding.
    pub issuers: Vec<String>,
}

impl Db {
    /// Insert `issuers` for `community` inside the caller's transaction.
    /// Existing rows are left as they are. No-op for an empty list.
    pub(crate) async fn insert_nip_fi_issuers_in_transaction(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        community: CommunityId,
        issuers: &[String],
    ) -> Result<u64> {
        if issuers.is_empty() {
            return Ok(0);
        }
        self.deletion_store()
            .guard_transaction(tx, community)
            .await?;
        let inserted = sqlx::query(
            r#"
            INSERT INTO community_nip_fi_issuers (community_id, issuer)
            SELECT $1, issuer FROM unnest($2::text[]) AS issuer
            ON CONFLICT DO NOTHING
            "#,
        )
        .bind(community.as_uuid())
        .bind(issuers)
        .execute(&mut **tx)
        .await?
        .rows_affected();
        Ok(inserted)
    }

    /// Bind `issuers` to an existing community. Returns the number of new rows.
    #[datastore_span(name = "bind_nip_fi_issuers", system = "postgresql")]
    pub async fn bind_nip_fi_issuers(
        &self,
        community: CommunityId,
        issuers: &[String],
    ) -> Result<u64> {
        let connection = crate::observability::acquire_writer(
            &self.pool,
            crate::observability::WriterOperation::Authorization,
        )
        .await?;
        let mut tx = Transaction::begin(connection, None).await?;
        let inserted = self
            .insert_nip_fi_issuers_in_transaction(&mut tx, community, issuers)
            .await?;
        tx.commit().await?;
        Ok(inserted)
    }

    /// Remove one issuer from a community. Returns whether a row was removed.
    #[datastore_span(name = "unbind_nip_fi_issuer", system = "postgresql")]
    pub async fn unbind_nip_fi_issuer(&self, community: CommunityId, issuer: &str) -> Result<bool> {
        let connection = crate::observability::acquire_writer(
            &self.pool,
            crate::observability::WriterOperation::Authorization,
        )
        .await?;
        let mut tx = Transaction::begin(connection, None).await?;
        self.deletion_store()
            .guard_transaction(&mut tx, community)
            .await?;
        let removed = sqlx::query(
            "DELETE FROM community_nip_fi_issuers WHERE community_id = $1 AND issuer = $2",
        )
        .bind(community.as_uuid())
        .bind(issuer)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        tx.commit().await?;
        Ok(removed > 0)
    }

    /// Read a community's binding by host, regardless of lifecycle state.
    /// `None` when no community has this host.
    #[datastore_span(name = "lookup_nip_fi_binding_by_host", system = "postgresql")]
    pub async fn lookup_nip_fi_binding_by_host(
        &self,
        normalized_host: &str,
    ) -> Result<Option<NipFiBindingRecord>> {
        let mut connection = crate::observability::acquire_writer(
            &self.pool,
            crate::observability::WriterOperation::Authorization,
        )
        .await?;
        let row = sqlx::query(
            r#"
            SELECT c.host,
                   (c.archived_at IS NULL
                    AND c.deletion_state = 'active'
                    AND c.deleted_at IS NULL) AS active,
                   COALESCE(
                       array_agg(b.issuer ORDER BY b.issuer COLLATE "C")
                           FILTER (WHERE b.issuer IS NOT NULL),
                       '{}'
                   ) AS issuers
            FROM communities c
            LEFT JOIN community_nip_fi_issuers b ON b.community_id = c.id
            WHERE lower(c.host) = lower($1)
            GROUP BY c.id
            "#,
        )
        .bind(normalized_host)
        .fetch_optional(&mut *connection)
        .await?;
        row.map(|row| {
            Ok(NipFiBindingRecord {
                host: row.try_get("host")?,
                active: row.try_get("active")?,
                issuers: row.try_get("issuers")?,
            })
        })
        .transpose()
    }

    /// Non-deleted communities with no binding whose host ends in
    /// `.<host_suffix>`. Callers apply the exact one-label rule.
    #[datastore_span(name = "list_unbound_nip_fi_communities", system = "postgresql")]
    pub async fn list_unbound_nip_fi_communities(
        &self,
        host_suffix: &str,
    ) -> Result<Vec<CommunityRecord>> {
        let mut connection = crate::observability::acquire_writer(
            &self.pool,
            crate::observability::WriterOperation::Authorization,
        )
        .await?;
        let rows = sqlx::query(
            r#"
            SELECT c.id, c.host
            FROM communities c
            WHERE right(lower(c.host), length($1) + 1) = '.' || lower($1)
              AND c.deletion_state = 'active'
              AND c.deleted_at IS NULL
              AND NOT EXISTS (
                  SELECT 1 FROM community_nip_fi_issuers b WHERE b.community_id = c.id
              )
            ORDER BY c.host
            "#,
        )
        .bind(host_suffix)
        .fetch_all(&mut *connection)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(CommunityRecord {
                    id: CommunityId::from_uuid(row.try_get("id")?),
                    host: row.try_get("host")?,
                })
            })
            .collect()
    }

    /// The subset of `hosts` whose community holds at least one binding.
    #[datastore_span(name = "nip_fi_bound_hosts", system = "postgresql")]
    pub async fn nip_fi_bound_hosts(&self, hosts: &[String]) -> Result<Vec<String>> {
        let mut connection = crate::observability::acquire_writer(
            &self.pool,
            crate::observability::WriterOperation::Bootstrap,
        )
        .await?;
        let bound = sqlx::query_scalar(
            r#"
            SELECT c.host
            FROM communities c
            WHERE lower(c.host) = ANY(SELECT lower(h) FROM unnest($1::text[]) AS h)
              AND EXISTS (
                  SELECT 1 FROM community_nip_fi_issuers b WHERE b.community_id = c.id
              )
            ORDER BY c.host
            "#,
        )
        .bind(hosts)
        .fetch_all(&mut *connection)
        .await?;
        Ok(bound)
    }
}

#[cfg(test)]
mod postgres_tests {
    use super::*;
    use crate::CreateCommunityWithOwnerResult;
    use sqlx::PgPool;
    use uuid::Uuid;

    const ISS: &str = "https://issuer.example/api/goose";
    const OWNER: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    async fn setup_db() -> Db {
        let pool = PgPool::connect(&crate::test_support::database_url())
            .await
            .expect("connect to test DB");
        Db::from_pool(pool)
    }

    fn unique_host(prefix: &str) -> String {
        format!("{prefix}-{}.bind.example", Uuid::new_v4().simple())
    }

    fn issuers() -> Vec<String> {
        vec![ISS.to_string()]
    }

    async fn bound(db: &Db, host: &str) -> Option<Vec<String>> {
        db.lookup_nip_fi_binding_by_host(host)
            .await
            .expect("lookup binding")
            .map(|record| record.issuers)
    }

    async fn community_exists(db: &Db, host: &str) -> bool {
        db.lookup_community_by_host_for_management(host)
            .await
            .expect("lookup community")
            .is_some()
    }

    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn create_with_owner_binds_in_the_insert_transaction() {
        let db = setup_db().await;
        let host = unique_host("create");
        let created = db
            .create_community_with_owner_and_nip_fi_issuers(&host, OWNER, &issuers())
            .await
            .expect("create");
        assert!(matches!(
            created,
            CreateCommunityWithOwnerResult::Created(_)
        ));
        assert_eq!(bound(&db, &host).await, Some(issuers()));

        // An identical retry returns the record without new rows.
        db.create_community_with_owner_and_nip_fi_issuers(
            &host,
            OWNER,
            &["https://other.example".to_string()],
        )
        .await
        .expect("retry");
        assert_eq!(bound(&db, &host).await, Some(issuers()));
    }

    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn ensure_binds_only_when_it_creates_the_row() {
        let db = setup_db().await;
        let host = unique_host("ensure");
        let first = db
            .ensure_configured_community_with_nip_fi_issuers(&host, &issuers())
            .await
            .expect("ensure");
        assert!(first.created);
        assert_eq!(bound(&db, &host).await, Some(issuers()));

        let again = db
            .ensure_configured_community_with_nip_fi_issuers(
                &host,
                &["https://other.example".to_string()],
            )
            .await
            .expect("ensure existing");
        assert!(!again.created);
        assert_eq!(bound(&db, &host).await, Some(issuers()));

        let bootstrap = unique_host("bootstrap");
        db.ensure_configured_community_for_bootstrap(&bootstrap, &issuers())
            .await
            .expect("bootstrap");
        assert_eq!(bound(&db, &bootstrap).await, Some(issuers()));
        let unbound = unique_host("bootstrap-unbound");
        db.ensure_configured_community_for_bootstrap(&unbound, &[])
            .await
            .expect("bootstrap unbound");
        assert_eq!(bound(&db, &unbound).await, Some(Vec::new()));
    }

    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn binding_insert_failure_rolls_back_the_community_on_every_path() {
        let db = setup_db().await;
        // An empty issuer violates the table's CHECK, failing the binding insert.
        let invalid = vec![String::new()];

        let create = unique_host("fail-create");
        assert!(db
            .create_community_with_owner_and_nip_fi_issuers(&create, OWNER, &invalid)
            .await
            .is_err());
        assert!(!community_exists(&db, &create).await);

        let ensure = unique_host("fail-ensure");
        assert!(db
            .ensure_configured_community_with_nip_fi_issuers(&ensure, &invalid)
            .await
            .is_err());
        assert!(!community_exists(&db, &ensure).await);

        let bootstrap = unique_host("fail-bootstrap");
        assert!(db
            .ensure_configured_community_for_bootstrap(&bootstrap, &invalid)
            .await
            .is_err());
        assert!(!community_exists(&db, &bootstrap).await);
    }

    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn backfill_queries_find_and_bind_unbound_suffix_hosts() {
        let db = setup_db().await;
        let suffix = format!("{}.suffix.example", Uuid::new_v4().simple());
        let unbound = format!("acme.{suffix}");
        let already = format!("bound.{suffix}");
        let elsewhere = unique_host("elsewhere");
        for (host, issuers) in [
            (&unbound, Vec::new()),
            (&already, issuers()),
            (&elsewhere, Vec::new()),
        ] {
            db.ensure_configured_community_with_nip_fi_issuers(host, &issuers)
                .await
                .expect("seed");
        }

        let found = db
            .list_unbound_nip_fi_communities(&suffix)
            .await
            .expect("list unbound");
        assert_eq!(
            found.iter().map(|c| c.host.as_str()).collect::<Vec<_>>(),
            [unbound.as_str()]
        );
        assert_eq!(
            db.bind_nip_fi_issuers(found[0].id, &issuers())
                .await
                .expect("bind"),
            1
        );
        assert_eq!(
            db.bind_nip_fi_issuers(found[0].id, &issuers())
                .await
                .expect("rebind"),
            0
        );
        assert!(db
            .list_unbound_nip_fi_communities(&suffix)
            .await
            .expect("relist")
            .is_empty());

        assert_eq!(
            db.nip_fi_bound_hosts(&[unbound.to_uppercase(), elsewhere.clone()])
                .await
                .expect("bound hosts"),
            std::slice::from_ref(&unbound)
        );
        assert!(db
            .unbind_nip_fi_issuer(found[0].id, ISS)
            .await
            .expect("unbind"));
        assert_eq!(bound(&db, &unbound).await, Some(Vec::new()));
        assert_eq!(bound(&db, &unique_host("missing")).await, None);
    }
}
