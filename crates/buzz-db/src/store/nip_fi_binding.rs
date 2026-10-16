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

/// A Postgres transaction snapshot (`pg_current_snapshot()`), recorded so the
/// relay can decide in process whether a committed transaction's effects are
/// included in a read taken under it (`pg_visible_in_snapshot` semantics).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PgSnapshot {
    xmin: u64,
    xmax: u64,
    in_progress: Vec<u64>,
}

impl PgSnapshot {
    /// Parse the `xmin:xmax:xip,...` text form of `pg_snapshot`.
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.splitn(3, ':');
        let xmin = parts.next()?.parse().ok()?;
        let xmax = parts.next()?.parse().ok()?;
        let in_progress = match parts.next()? {
            "" => Vec::new(),
            list => list
                .split(',')
                .map(str::parse)
                .collect::<std::result::Result<_, _>>()
                .ok()?,
        };
        Some(Self {
            xmin,
            xmax,
            in_progress,
        })
    }

    /// Whether committed transaction `xid` is visible to a read under this
    /// snapshot: below `xmin`, or below `xmax` and not in progress.
    pub fn is_visible(&self, xid: u64) -> bool {
        xid < self.xmin || (xid < self.xmax && !self.in_progress.contains(&xid))
    }
}

/// One consistent read of every active registry-filtered binding.
#[derive(Debug)]
pub struct NipFiSnapshotLoad {
    /// The snapshot the scan ran under.
    pub snapshot: PgSnapshot,
    /// `(host, issuer)` rows, one per community-issuer pair.
    pub rows: Vec<(String, String)>,
}

/// A streamed read produced more rows than its cap allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("row cap of {0} exceeded")]
pub struct RowCapExceeded(pub usize);

/// Collect at most `cap` rows from `rows`, pulling at most `cap + 1`: the
/// `cap + 1`st row fails the whole read. Shared by the snapshot load and the
/// admission miss lookup so both stop streaming at the same bound.
pub async fn collect_capped<T, E, S>(
    rows: S,
    cap: usize,
) -> std::result::Result<std::result::Result<Vec<T>, RowCapExceeded>, E>
where
    S: futures_util::Stream<Item = std::result::Result<T, E>>,
{
    use futures_util::StreamExt;
    let mut rows = std::pin::pin!(rows);
    let mut out = Vec::new();
    while let Some(row) = rows.next().await {
        if out.len() == cap {
            return Ok(Err(RowCapExceeded(cap)));
        }
        out.push(row?);
    }
    Ok(Ok(out))
}

impl Db {
    /// Read every binding of an active community to an issuer in `registry`,
    /// on the primary, inside one `REPEATABLE READ` transaction whose first
    /// statement records its snapshot. `after_snapshot` runs between that
    /// statement and the scan. A read over `max_rows` rows stops at the
    /// `max_rows + 1`st and fails.
    #[datastore_span(name = "load_nip_fi_binding_snapshot", system = "postgresql")]
    pub async fn load_nip_fi_binding_snapshot(
        &self,
        registry: &[String],
        max_rows: usize,
        after_snapshot: impl std::future::Future<Output = ()> + Send,
    ) -> Result<std::result::Result<NipFiSnapshotLoad, RowCapExceeded>> {
        let connection = crate::observability::acquire_writer(
            &self.pool,
            crate::observability::WriterOperation::Authorization,
        )
        .await?;
        let mut tx = Transaction::begin(connection, None).await?;
        // Sets the level only; the snapshot is taken by the next statement.
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *tx)
            .await?;
        let snapshot: String = sqlx::query_scalar("SELECT pg_current_snapshot()::text")
            .fetch_one(&mut *tx)
            .await?;
        let snapshot = PgSnapshot::parse(&snapshot)
            .ok_or_else(|| crate::DbError::InvalidData("unparseable pg_snapshot".into()))?;
        after_snapshot.await;
        let rows = {
            let stream = sqlx::query_as::<_, (String, String)>(
                r#"
                SELECT c.host, b.issuer
                FROM community_nip_fi_issuers b
                JOIN communities c ON c.id = b.community_id
                WHERE b.issuer = ANY($1)
                  AND c.archived_at IS NULL
                  AND c.deletion_state = 'active'
                  AND c.deleted_at IS NULL
                "#,
            )
            .bind(registry)
            .fetch(&mut *tx);
            collect_capped(stream, max_rows).await?
        };
        tx.commit().await?;
        Ok(rows.map(|rows| NipFiSnapshotLoad { snapshot, rows }))
    }

    /// The registry-filtered issuers bound to the active community at
    /// `normalized_host`, on the primary. Streams at most `registry.len() + 1`
    /// rows; more than `registry.len()` fails the lookup. Empty when the host
    /// is unknown, inactive, or bound only to unregistered issuers.
    #[datastore_span(name = "lookup_nip_fi_admission_binding", system = "postgresql")]
    pub async fn lookup_nip_fi_admission_binding(
        &self,
        normalized_host: &str,
        registry: &[String],
    ) -> Result<std::result::Result<Vec<String>, RowCapExceeded>> {
        let mut connection = crate::observability::acquire_writer(
            &self.pool,
            crate::observability::WriterOperation::Authorization,
        )
        .await?;
        let cap = registry.len();
        let stream = sqlx::query_scalar::<_, String>(
            r#"
            SELECT b.issuer
            FROM community_nip_fi_issuers b
            JOIN communities c ON c.id = b.community_id
            WHERE lower(c.host) = lower($1)
              AND b.issuer = ANY($2)
              AND c.archived_at IS NULL
              AND c.deletion_state = 'active'
              AND c.deleted_at IS NULL
            LIMIT $3
            "#,
        )
        .bind(normalized_host)
        .bind(registry)
        .bind(i64::try_from(cap + 1).unwrap_or(i64::MAX))
        .fetch(&mut *connection);
        Ok(collect_capped(stream, cap).await?)
    }
}

/// The creating transaction's ID (`pg_current_xact_id()`), read inside it.
pub(crate) async fn current_xact_id(tx: &mut Transaction<'_, Postgres>) -> Result<u64> {
    let xid: String = sqlx::query_scalar("SELECT pg_current_xact_id()::text")
        .fetch_one(&mut **tx)
        .await?;
    xid.parse()
        .map_err(|_| crate::DbError::InvalidData("unparseable xid8".into()))
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

    /// Move `host`'s community out of `active` the way the deletion executor
    /// does: the tombstone trigger admits the state change only under the
    /// executor's community and fence-generation settings.
    async fn start_deleting(db: &Db, host: &str) {
        let mut tx = db.pool.begin().await.expect("begin");
        let id: Uuid = sqlx::query_scalar("SELECT id FROM communities WHERE host = $1")
            .bind(host)
            .fetch_one(&mut *tx)
            .await
            .expect("community id");
        sqlx::query("SELECT set_config('buzz.deletion_executor_community', $1, true), set_config('buzz.deletion_fence_generation', '1', true)")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await
            .expect("executor settings");
        sqlx::query("UPDATE communities SET deletion_state = 'quiescing', deletion_fence_generation = 1 WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .expect("start deletion");
        tx.commit().await.expect("commit");
    }

    async fn snapshot_hosts(db: &Db, registry: &[String]) -> Vec<String> {
        db.load_nip_fi_binding_snapshot(registry, 100_000, async {})
            .await
            .expect("snapshot")
            .expect("under cap")
            .rows
            .into_iter()
            .map(|(host, _)| host)
            .collect()
    }

    async fn admission(db: &Db, host: &str, registry: &[String]) -> Vec<String> {
        db.lookup_nip_fi_admission_binding(host, registry)
            .await
            .expect("admission lookup")
            .expect("under cap")
    }

    // Criterion 14(a): archived and deleting communities drop out of both the
    // snapshot load and the admission miss lookup; an active control stays.
    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn snapshot_and_admission_lookup_skip_archived_and_deleting_communities() {
        let db = setup_db().await;
        let registry = issuers();
        let [archived, deleting, control] = ["archived", "deleting", "control"].map(unique_host);
        for host in [&archived, &deleting, &control] {
            db.ensure_configured_community_with_nip_fi_issuers(host, &registry)
                .await
                .expect("seed");
            assert!(snapshot_hosts(&db, &registry).await.contains(host));
            assert_eq!(admission(&db, host, &registry).await, registry);
        }

        sqlx::query("UPDATE communities SET archived_at = now() WHERE host = $1")
            .bind(&archived)
            .execute(&db.pool)
            .await
            .expect("archive");
        start_deleting(&db, &deleting).await;

        let hosts = snapshot_hosts(&db, &registry).await;
        for gone in [&archived, &deleting] {
            assert!(!hosts.contains(gone), "{gone} must leave the snapshot");
            assert!(admission(&db, gone, &registry).await.is_empty(), "{gone}");
        }
        assert!(hosts.contains(&control));
        assert_eq!(admission(&db, &control, &registry).await, registry);
    }

    // Criterion 17(a)(1): only registered issuers come back, and a host bound
    // only to unregistered issuers yields zero rows from the real SQL.
    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn admission_lookup_returns_only_registered_issuers() {
        let db = setup_db().await;
        let other = "https://unregistered.example".to_string();
        let mixed = unique_host("mixed");
        let foreign = unique_host("foreign");
        db.ensure_configured_community_with_nip_fi_issuers(
            &mixed,
            &[ISS.to_string(), other.clone()],
        )
        .await
        .expect("seed mixed");
        db.ensure_configured_community_with_nip_fi_issuers(&foreign, std::slice::from_ref(&other))
            .await
            .expect("seed foreign");

        assert_eq!(
            admission(&db, &mixed.to_uppercase(), &issuers()).await,
            issuers()
        );
        assert!(admission(&db, &foreign, &issuers()).await.is_empty());
        assert!(!snapshot_hosts(&db, &issuers()).await.contains(&foreign));
    }

    async fn inserting_xid(db: &Db, host: &str) -> u64 {
        sqlx::query_scalar::<_, String>(
            "SELECT b.xmin::text FROM community_nip_fi_issuers b \
             JOIN communities c ON c.id = b.community_id WHERE c.host = $1",
        )
        .bind(host)
        .fetch_one(&db.pool)
        .await
        .expect("binding xmin")
        .parse()
        .expect("xid")
    }

    // Criterion 15 (real SQL): a creation committed after the load records
    // its snapshot but before the scan is absent from the rows, and the
    // recorded snapshot reports it invisible, so the overlay keeps it. A
    // creation committed before the snapshot is in both. Weakening the
    // transaction to READ COMMITTED puts the late row in the scan.
    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn snapshot_excludes_creations_committed_after_it_is_recorded() {
        let db = setup_db().await;
        let early = unique_host("early");
        let late = unique_host("late");
        db.ensure_configured_community_with_nip_fi_issuers(&early, &issuers())
            .await
            .expect("seed early");
        let load = db
            .load_nip_fi_binding_snapshot(&issuers(), 100_000, async {
                db.ensure_configured_community_with_nip_fi_issuers(&late, &issuers())
                    .await
                    .expect("seed late");
            })
            .await
            .expect("snapshot")
            .expect("under cap");
        let hosts: Vec<_> = load.rows.iter().map(|(host, _)| host).collect();
        assert!(hosts.contains(&&early));
        assert!(!hosts.contains(&&late), "late creation must not be scanned");
        assert!(load.snapshot.is_visible(inserting_xid(&db, &early).await));
        assert!(!load.snapshot.is_visible(inserting_xid(&db, &late).await));
    }
}
