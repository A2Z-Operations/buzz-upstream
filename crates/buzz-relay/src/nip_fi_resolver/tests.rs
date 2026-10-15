//! Resolver ordering, staleness, and bounds tests (plan criteria 7f, 9, 13,
//! 15, 16, 17). The store is an in-memory fake that counts every call; the
//! clock is manual. The Postgres-backed visibility semantics are covered by
//! `PgSnapshot::is_visible` here and by the store's `postgres_tests::`.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use buzz_auth::{DenialClass, ProvisionBindingPolicy};
use buzz_db::{NipFiSnapshotLoad, PgSnapshot, RowCapExceeded};
use futures_util::future::{BoxFuture, FutureExt};

use super::hooks::{
    CREATION_BEFORE_OVERLAY, MISS_AFTER_READ, REFRESH_AFTER_SNAPSHOT, REFRESH_BEFORE_PUBLISH,
};
use super::*;

const ISS: &str = "https://app.builderlab.xyz/api/goose";
const UNAVAILABLE: Result<(), DenialClass> = Err(DenialClass::AuthorizationUnavailable);

fn policy() -> ProvisionBindingPolicy {
    ProvisionBindingPolicy::parse(
        &serde_json::json!({"host_suffix": "communities.buzz.xyz", "authorized_issuers": [ISS]})
            .to_string(),
        &serde_json::json!([
            {"issuer": ISS, "command_audiences": ["https://communities.buzz.xyz"], "token_class": "nip-fi+jwt"}
        ])
        .to_string(),
    )
    .expect("valid policy")
}

fn host(label: &str) -> String {
    format!("{label}.communities.buzz.xyz")
}

#[derive(Default)]
struct ManualClock(AtomicU64);

impl ManualClock {
    fn set(&self, at: Duration) {
        self.0.store(at.as_millis() as u64, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Duration {
        Duration::from_millis(self.0.load(Ordering::SeqCst))
    }
}

/// In-memory bindings plus the snapshot the next load reports.
struct FakeStore {
    hosts: Mutex<Vec<String>>,
    snapshot: Mutex<String>,
    fail_loads: std::sync::atomic::AtomicBool,
    lookup_fails: std::sync::atomic::AtomicBool,
    loads: AtomicUsize,
    lookups: Mutex<Vec<String>>,
    lookup_delay: Mutex<Duration>,
    load_delay: Mutex<Duration>,
}

impl FakeStore {
    fn new(hosts: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            hosts: Mutex::new(hosts.iter().map(|l| host(l)).collect()),
            snapshot: Mutex::new("100:100:".into()),
            fail_loads: false.into(),
            lookup_fails: false.into(),
            loads: AtomicUsize::new(0),
            lookups: Mutex::new(Vec::new()),
            lookup_delay: Mutex::new(Duration::ZERO),
            load_delay: Mutex::new(Duration::ZERO),
        })
    }
    fn add(&self, label: &str) {
        self.hosts.lock().unwrap().push(host(label));
    }
    fn remove(&self, label: &str) {
        self.hosts.lock().unwrap().retain(|h| h != &host(label));
    }
    fn set_snapshot(&self, text: &str) {
        *self.snapshot.lock().unwrap() = text.into();
    }
    fn lookups_for(&self, label: &str) -> usize {
        let h = host(label);
        self.lookups
            .lock()
            .unwrap()
            .iter()
            .filter(|l| **l == h)
            .count()
    }
    fn total_lookups(&self) -> usize {
        self.lookups.lock().unwrap().len()
    }
}

impl BindingSource for FakeStore {
    fn load<'a>(
        &'a self,
        _registry: &'a [String],
        max_rows: usize,
        after_snapshot: BoxFuture<'a, ()>,
    ) -> BoxFuture<'a, Result<Result<NipFiSnapshotLoad, RowCapExceeded>, String>> {
        async move {
            self.loads.fetch_add(1, Ordering::SeqCst);
            let snapshot = PgSnapshot::parse(&self.snapshot.lock().unwrap()).unwrap();
            after_snapshot.await;
            let delay = *self.load_delay.lock().unwrap();
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            if self.fail_loads.load(Ordering::SeqCst) {
                return Err("load failed".into());
            }
            let rows: Vec<(String, String)> = self
                .hosts
                .lock()
                .unwrap()
                .iter()
                .map(|h| (h.clone(), ISS.to_owned()))
                .collect();
            if rows.len() > max_rows {
                return Ok(Err(RowCapExceeded(max_rows)));
            }
            Ok(Ok(NipFiSnapshotLoad { snapshot, rows }))
        }
        .boxed()
    }

    fn lookup<'a>(
        &'a self,
        host: &'a str,
        _registry: &'a [String],
    ) -> BoxFuture<'a, Result<Result<Vec<String>, RowCapExceeded>, String>> {
        async move {
            self.lookups.lock().unwrap().push(host.to_owned());
            let delay = *self.lookup_delay.lock().unwrap();
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            if self.lookup_fails.load(Ordering::SeqCst) {
                return Err("pool closed".into());
            }
            let found = self.hosts.lock().unwrap().iter().any(|h| h == host);
            Ok(Ok(if found {
                vec![ISS.to_owned()]
            } else {
                Vec::new()
            }))
        }
        .boxed()
    }
}

struct Fixture {
    store: Arc<FakeStore>,
    clock: Arc<ManualClock>,
    resolver: Arc<DatabaseResolver>,
}

fn fixture_with(hosts: &[&str], bounds: ResolverBounds) -> Fixture {
    let store = FakeStore::new(hosts);
    let clock = Arc::new(ManualClock::default());
    let resolver = Arc::new(DatabaseResolver::with_clock(
        Arc::clone(&store) as Arc<dyn BindingSource>,
        policy(),
        vec![ISS.to_owned()],
        bounds,
        Arc::clone(&clock) as Arc<dyn Clock>,
    ));
    Fixture {
        store,
        clock,
        resolver,
    }
}

fn fixture(hosts: &[&str]) -> Fixture {
    fixture_with(hosts, ResolverBounds::default())
}

impl Fixture {
    async fn resolve(&self, label: &str) -> Result<(), DenialClass> {
        self.resolver.resolve(&host(label)).await.map(|_| ())
    }
}

fn secs(s: f64) -> Duration {
    Duration::from_secs_f64(s)
}

// ── Criterion 12: nothing resolves before the first successful load ─────────

#[tokio::test]
async fn unloaded_resolver_denies_and_failed_or_over_cap_load_keeps_it_unloaded() {
    let f = fixture_with(
        &["a", "b"],
        ResolverBounds {
            snapshot_max_rows: 1,
            ..ResolverBounds::default()
        },
    );
    assert!(!f.resolver.is_loaded());
    assert_eq!(f.resolve("a").await, UNAVAILABLE);
    assert_eq!(f.resolver.refresh().await, Err(LoadFailure::OverCap));
    f.store.fail_loads.store(true, Ordering::SeqCst);
    assert!(matches!(
        f.resolver.refresh().await,
        Err(LoadFailure::Store(_))
    ));
    assert!(!f.resolver.is_loaded());
    assert_eq!(
        f.store.total_lookups(),
        0,
        "unloaded resolver never looks up"
    );
}

// ── Criterion 13: staleness ──────────────────────────────────────────────────

#[tokio::test]
async fn failed_refreshes_serve_last_snapshot_for_two_minutes_then_fail_closed() {
    let f = fixture(&["a"]);
    f.resolver.refresh().await.unwrap();
    f.store.fail_loads.store(true, Ordering::SeqCst);

    f.clock.set(secs(60.0));
    assert!(f.resolver.refresh().await.is_err());
    // A successful miss and a local creation do not move the deadline.
    f.store.add("m");
    assert_eq!(f.resolve("m").await, Ok(()));
    f.resolver
        .record_creation(&host("c"), &[ISS.to_owned()], 500)
        .await;

    f.clock.set(secs(119.0));
    assert_eq!(f.resolve("a").await, Ok(()));
    assert_eq!(f.store.lookups_for("a"), 0, "snapshot host never looks up");

    f.clock.set(secs(121.0));
    for label in ["a", "c", "m", "unknown"] {
        assert_eq!(f.resolve(label).await, UNAVAILABLE, "{label}");
    }

    f.store.fail_loads.store(false, Ordering::SeqCst);
    f.resolver.refresh().await.unwrap();
    assert_eq!(f.resolve("a").await, Ok(()));
}

#[tokio::test]
async fn miss_read_held_across_the_deadline_is_denied() {
    let f = fixture(&["a"]);
    f.resolver.refresh().await.unwrap();
    f.store.add("late");
    f.clock.set(secs(119.9));
    let gate = f.resolver.hooks.arm(MISS_AFTER_READ);
    let pending = tokio::spawn({
        let resolver = Arc::clone(&f.resolver);
        async move { resolver.resolve(&host("late")).await.map(|_| ()) }
    });
    gate.reached().await;
    f.clock.set(secs(120.1));
    gate.release();
    assert_eq!(pending.await.unwrap(), UNAVAILABLE);
}

#[tokio::test]
async fn timed_out_refresh_publishes_nothing_and_keeps_the_deadline() {
    let f = fixture_with(
        &["a"],
        ResolverBounds {
            refresh_timeout: Duration::from_millis(20),
            ..ResolverBounds::default()
        },
    );
    f.resolver.refresh().await.unwrap();
    f.store.remove("a");
    f.store.add("b");
    *f.store.load_delay.lock().unwrap() = Duration::from_millis(200);
    f.clock.set(secs(100.0));
    assert_eq!(f.resolver.refresh().await, Err(LoadFailure::Timeout));
    // Retained snapshot unchanged: "a" still resolves without a lookup.
    assert_eq!(f.resolve("a").await, Ok(()));
    assert_eq!(f.store.lookups_for("a"), 0);
    f.clock.set(secs(121.0));
    assert_eq!(f.resolve("a").await, UNAVAILABLE);
}

#[tokio::test]
async fn failed_miss_lookup_denies_and_stores_no_negative_entry() {
    let f = fixture(&["a"]);
    f.resolver.refresh().await.unwrap();
    f.store.lookup_fails.store(true, Ordering::SeqCst);
    assert_eq!(f.resolve("x").await, UNAVAILABLE);
    assert_eq!(f.resolver.negative_len(), 0);
}

// ── Criterion 14: refresh removes bindings; 40s removal bound ───────────────

#[tokio::test]
async fn refresh_removes_unbound_communities_and_held_positive_miss_is_not_stored() {
    let f = fixture(&["a", "b", "control"]);
    f.resolver.refresh().await.unwrap();
    f.store.remove("a");
    f.store.remove("b");
    assert_eq!(f.resolve("a").await, Ok(()), "before refresh");
    f.resolver.refresh().await.unwrap();
    assert_eq!(f.resolve("a").await, UNAVAILABLE);
    assert_eq!(f.resolve("b").await, UNAVAILABLE);
    assert_eq!(f.resolve("control").await, Ok(()));

    // (c) A miss reads "h" active and is held; "h" is removed and a refresh
    // publishes; the held answer is returned once and never stored.
    f.store.add("h");
    let gate = f.resolver.hooks.arm(MISS_AFTER_READ);
    let pending = tokio::spawn({
        let resolver = Arc::clone(&f.resolver);
        async move { resolver.resolve(&host("h")).await.map(|_| ()) }
    });
    gate.reached().await;
    f.resolver.hooks.disarm(MISS_AFTER_READ);
    f.store.remove("h");
    f.resolver.refresh().await.unwrap();
    gate.release();
    assert_eq!(pending.await.unwrap(), Ok(()));
    assert_eq!(f.resolve("h").await, UNAVAILABLE);
}

#[tokio::test]
async fn overlay_entry_created_after_refresh_snapshot_survives_until_a_snapshot_sees_it() {
    // (b): refresh fixes its snapshot (xmax 100), H is created in xid 100 and
    // archived; that refresh keeps H in the overlay, the next one drops it.
    let f = fixture(&["a"]);
    f.resolver.refresh().await.unwrap();
    let gate = f.resolver.hooks.arm(REFRESH_AFTER_SNAPSHOT);
    let refresh = tokio::spawn({
        let resolver = Arc::clone(&f.resolver);
        async move { resolver.refresh().await }
    });
    gate.reached().await;
    f.resolver.hooks.disarm(REFRESH_AFTER_SNAPSHOT);
    f.resolver
        .record_creation(&host("h"), &[ISS.to_owned()], 100)
        .await;
    assert_eq!(f.resolve("h").await, Ok(()));
    gate.release();
    refresh.await.unwrap().unwrap();
    assert_eq!(
        f.resolver.overlay_len(),
        1,
        "xid 100 not visible to 100:100:"
    );
    // Archived before the next snapshot: that snapshot sees both.
    f.store.set_snapshot("101:101:");
    f.resolver.refresh().await.unwrap();
    assert_eq!(f.resolver.overlay_len(), 0);
    assert_eq!(f.resolve("h").await, UNAVAILABLE);
    assert_eq!(f.store.lookups_for("h"), 1);
}

#[tokio::test(start_paused = true)]
async fn unbinding_after_a_snapshot_is_enforced_within_forty_seconds() {
    let f = fixture(&["a", "gone"]);
    f.resolver.refresh().await.unwrap();
    f.store.remove("gone");
    // Next refresh starts at +30s and is held for just under its 10s limit.
    f.clock.set(secs(30.0));
    let gate = f.resolver.hooks.arm(REFRESH_BEFORE_PUBLISH);
    let refresh = tokio::spawn({
        let resolver = Arc::clone(&f.resolver);
        async move { resolver.refresh().await }
    });
    gate.reached().await;
    tokio::time::advance(Duration::from_millis(9_900)).await;
    f.clock.set(secs(39.9));
    assert_eq!(
        f.resolve("gone").await,
        Ok(()),
        "still resolves before publish"
    );
    gate.release();
    refresh.await.unwrap().unwrap();
    assert_eq!(f.resolve("gone").await, UNAVAILABLE);
}

// ── Criterion 15: creation vs refresh, serialized publication ──────────────

#[tokio::test]
async fn creation_during_a_refresh_is_kept_after_the_swap_and_dropped_once_read() {
    // (a) and (c): the refresh read excludes H; H is created while the
    // refresh is paused before publishing; H resolves from memory after.
    let f = fixture(&["a"]);
    f.resolver.refresh().await.unwrap();
    let gate = f.resolver.hooks.arm(REFRESH_BEFORE_PUBLISH);
    let refresh = tokio::spawn({
        let resolver = Arc::clone(&f.resolver);
        async move { resolver.refresh().await }
    });
    gate.reached().await;
    f.resolver.hooks.disarm(REFRESH_BEFORE_PUBLISH);
    f.store.add("h");
    f.resolver
        .record_creation(&host("h"), &[ISS.to_owned()], 100)
        .await;
    gate.release();
    refresh.await.unwrap().unwrap();
    assert_eq!(f.resolve("h").await, Ok(()));
    assert_eq!(f.store.lookups_for("h"), 0, "resolved from memory");

    f.store.set_snapshot("101:101:");
    f.resolver.refresh().await.unwrap();
    assert_eq!(f.resolver.overlay_len(), 0, "own read includes H");
    assert_eq!(f.resolve("h").await, Ok(()));
    assert_eq!(f.store.lookups_for("h"), 0);
}

#[tokio::test]
async fn late_overlay_insert_is_skipped_when_a_published_snapshot_saw_the_creation() {
    // (b)
    let f = fixture(&["a"]);
    f.resolver.refresh().await.unwrap();
    let gate = f.resolver.hooks.arm(CREATION_BEFORE_OVERLAY);
    let creation = tokio::spawn({
        let resolver = Arc::clone(&f.resolver);
        async move {
            resolver
                .record_creation(&host("h"), &[ISS.to_owned()], 100)
                .await
        }
    });
    gate.reached().await;
    f.store.add("h");
    f.store.set_snapshot("101:101:");
    f.resolver.refresh().await.unwrap();
    f.store.remove("h");
    f.store.set_snapshot("102:102:");
    f.resolver.refresh().await.unwrap();
    gate.release();
    creation.await.unwrap();
    assert_eq!(f.resolver.overlay_len(), 0);
    assert_eq!(f.resolve("h").await, UNAVAILABLE);
}

#[tokio::test]
async fn negative_lookup_racing_a_creation_or_swap_stores_nothing() {
    // (d), and criterion 7(f).
    for swap in [false, true] {
        let f = fixture(&["a"]);
        f.resolver.refresh().await.unwrap();
        let gate = f.resolver.hooks.arm(MISS_AFTER_READ);
        let pending = tokio::spawn({
            let resolver = Arc::clone(&f.resolver);
            async move { resolver.resolve(&host("h")).await.map(|_| ()) }
        });
        gate.reached().await;
        f.resolver.hooks.disarm(MISS_AFTER_READ);
        f.store.add("h");
        if swap {
            f.resolver.refresh().await.unwrap();
        } else {
            f.resolver
                .record_creation(&host("h"), &[ISS.to_owned()], 100)
                .await;
        }
        gate.release();
        assert_eq!(pending.await.unwrap(), UNAVAILABLE, "answered as read");
        assert_eq!(f.resolver.negative_len(), 0, "swap={swap}");
        assert_eq!(f.resolve("h").await, Ok(()), "swap={swap}");
    }
}

#[tokio::test]
async fn in_progress_creation_below_xmax_stays_in_the_overlay() {
    // (e): C has xid 50, a later xid committed so xmax is 60, and the
    // refresh snapshot lists 50 as in progress. C's overlay entry survives.
    let f = fixture(&["a"]);
    f.store.set_snapshot("40:40:");
    f.resolver.refresh().await.unwrap();
    f.store.set_snapshot("50:60:50");
    let gate = f.resolver.hooks.arm(REFRESH_AFTER_SNAPSHOT);
    let refresh = tokio::spawn({
        let resolver = Arc::clone(&f.resolver);
        async move { resolver.refresh().await }
    });
    gate.reached().await;
    f.resolver.hooks.disarm(REFRESH_AFTER_SNAPSHOT);
    f.resolver
        .record_creation(&host("h"), &[ISS.to_owned()], 50)
        .await;
    gate.release();
    refresh.await.unwrap().unwrap();
    assert_eq!(f.resolve("h").await, Ok(()));
    assert_eq!(f.store.lookups_for("h"), 0);
}

// ── Criterion 16: overlay bound under sustained creation ───────────────────

#[tokio::test]
async fn overlay_stays_bounded_while_refresh_fails() {
    let f = fixture_with(
        &["a", "b"],
        ResolverBounds {
            overlay_max: 100,
            ..ResolverBounds::default()
        },
    );
    f.resolver.refresh().await.unwrap();
    f.store.fail_loads.store(true, Ordering::SeqCst);
    for i in 0..500u64 {
        let label = format!("n{i}");
        f.store.add(&label);
        f.resolver
            .record_creation(&host(&label), &[ISS.to_owned()], 1_000 + i)
            .await;
        assert!(f.resolver.overlay_len() <= 100);
        assert_eq!(f.resolve("a").await, Ok(()));
    }
    for i in 0..500 {
        let label = format!("n{i}");
        assert_eq!(f.resolve(&label).await, Ok(()));
        let expected = usize::from(i >= 100);
        assert_eq!(f.store.lookups_for(&label), expected, "{label}");
    }
    assert_eq!(f.store.lookups_for("a") + f.store.lookups_for("b"), 0);

    f.clock.set(secs(121.0));
    f.resolver
        .record_creation(&host("late"), &[ISS.to_owned()], 9_999)
        .await;
    assert!(f.resolver.overlay_len() <= 100);
    assert_eq!(f.resolve("a").await, UNAVAILABLE);

    f.store.fail_loads.store(false, Ordering::SeqCst);
    f.store.set_snapshot("100000:100000:");
    f.resolver.refresh().await.unwrap();
    assert_eq!(f.resolver.overlay_len(), 0);
    let before = f.store.total_lookups();
    for i in 0..500 {
        assert_eq!(f.resolve(&format!("n{i}")).await, Ok(()));
    }
    assert_eq!(f.store.total_lookups(), before);
}

// ── Criterion 9: miss-path bounds under a flood ─────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flood_of_unknown_hosts_respects_every_cap_and_spares_snapshot_hosts() {
    let bounds = ResolverBounds {
        max_inflight: 4,
        max_pending_hosts: 8,
        max_waiters: 16,
        negative_cache_max: 32,
        permit_wait: Duration::from_millis(5),
        lookup_timeout: Duration::from_millis(500),
        ..ResolverBounds::default()
    };
    let f = fixture_with(&["a", "b"], bounds.clone());
    f.resolver.refresh().await.unwrap();
    *f.store.lookup_delay.lock().unwrap() = Duration::from_millis(20);

    let peak = Arc::new(Mutex::new((0usize, 0usize, 0usize, 0usize)));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let monitor = tokio::spawn({
        let (resolver, peak, stop) = (
            Arc::clone(&f.resolver),
            Arc::clone(&peak),
            Arc::clone(&stop),
        );
        async move {
            while !stop.load(Ordering::SeqCst) {
                {
                    let mut p = peak.lock().unwrap();
                    p.0 = p.0.max(resolver.inflight_now());
                    p.1 = p.1.max(resolver.pending_len());
                    p.2 = p.2.max(resolver.waiters_now());
                    p.3 = p.3.max(resolver.negative_len());
                }
                tokio::task::yield_now().await;
            }
        }
    });
    let flood: Vec<_> = (0..400)
        .map(|i| {
            let resolver = Arc::clone(&f.resolver);
            tokio::spawn(async move { resolver.resolve(&host(&format!("rand{i}"))).await.is_err() })
        })
        .collect();
    f.store.add("new");
    f.resolver
        .record_creation(&host("new"), &[ISS.to_owned()], 100)
        .await;
    for _ in 0..50 {
        assert_eq!(f.resolve("a").await, Ok(()));
        assert_eq!(f.resolve("b").await, Ok(()));
        assert_eq!(f.resolve("new").await, Ok(()));
    }
    for task in flood {
        assert!(task.await.unwrap(), "every made-up host is denied");
    }
    stop.store(true, Ordering::SeqCst);
    monitor.await.unwrap();
    let p = *peak.lock().unwrap();
    assert!(p.0 <= bounds.max_inflight, "inflight {}", p.0);
    assert!(p.1 <= bounds.max_pending_hosts, "pending {}", p.1);
    assert!(p.2 <= bounds.max_waiters, "waiters {}", p.2);
    assert!(p.3 <= bounds.negative_cache_max, "negative {}", p.3);
    assert_eq!(
        f.store.lookups_for("a") + f.store.lookups_for("b") + f.store.lookups_for("new"),
        0
    );
}

// ── Criterion 17: map lifetime ──────────────────────────────────────────────

#[tokio::test]
async fn old_snapshot_maps_are_dropped_while_admissions_keep_their_bindings() {
    let f = fixture(&["a"]);
    f.resolver.refresh().await.unwrap();
    let mut held = Vec::new();
    let mut weak_maps = Vec::new();
    for i in 0..10u64 {
        held.push(f.resolver.resolve(&host("a")).await.unwrap());
        weak_maps.push(Arc::downgrade(
            f.resolver.published.load_full().as_ref().unwrap(),
        ));
        f.store.set_snapshot(&format!("{0}:{0}:", 200 + i));
        f.resolver.refresh().await.unwrap();
    }
    let alive = weak_maps.iter().filter(|w| w.strong_count() > 0).count();
    assert_eq!(alive, 0, "only the published map is alive");
    assert!(held
        .iter()
        .all(|b| b.expected_aud() == "https://a.communities.buzz.xyz"));
}

// ── Pre-filter ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn non_suffix_reserved_and_multi_label_hosts_never_reach_the_store() {
    let f = fixture(&["a"]);
    f.resolver.refresh().await.unwrap();
    for raw in [
        "communities.buzz.xyz",
        "a.b.communities.buzz.xyz",
        "evil.example",
        "a.communities.buzz.xyz:8443",
    ] {
        assert!(f.resolver.resolve(raw).await.is_err(), "{raw}");
    }
    assert_eq!(f.store.total_lookups(), 0);
}

#[test]
fn snapshot_visibility_honours_the_in_progress_list() {
    let s = PgSnapshot::parse("50:60:50,55").unwrap();
    assert!(s.is_visible(49));
    assert!(!s.is_visible(50));
    assert!(s.is_visible(52));
    assert!(!s.is_visible(55));
    assert!(!s.is_visible(60));
}

/// §8 bounds: unset or blank variables take the defaults, a set value
/// overrides only its own bound, and out-of-range or non-numeric values fail
/// startup rather than silently falling back.
#[test]
fn resolver_bounds_from_env_defaults_overrides_and_rejects() {
    const VARS: [&str; 9] = [
        "BUZZ_NIP_FI_RESOLVER_NEGATIVE_CACHE_MAX",
        "BUZZ_NIP_FI_RESOLVER_MAX_INFLIGHT",
        "BUZZ_NIP_FI_RESOLVER_MAX_PENDING_HOSTS",
        "BUZZ_NIP_FI_RESOLVER_MAX_WAITERS",
        "BUZZ_NIP_FI_RESOLVER_PERMIT_WAIT_MS",
        "BUZZ_NIP_FI_RESOLVER_LOOKUP_TIMEOUT_MS",
        "BUZZ_NIP_FI_SNAPSHOT_MAX_ROWS",
        "BUZZ_NIP_FI_OVERLAY_MAX",
        "BUZZ_NIP_FI_SNAPSHOT_REFRESH_TIMEOUT_MS",
    ];
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            for v in VARS {
                std::env::remove_var(v);
            }
        }
    }
    let _lock = crate::nip_fi_config::NIP_FI_ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _clear = Clear;
    for v in VARS {
        std::env::remove_var(v);
    }

    assert_eq!(
        ResolverBounds::from_env().unwrap(),
        ResolverBounds::default()
    );

    std::env::set_var("BUZZ_NIP_FI_OVERLAY_MAX", "250");
    std::env::set_var("BUZZ_NIP_FI_SNAPSHOT_REFRESH_TIMEOUT_MS", "7500");
    std::env::set_var("BUZZ_NIP_FI_RESOLVER_MAX_INFLIGHT", " ");
    let bounds = ResolverBounds::from_env().unwrap();
    assert_eq!(bounds.overlay_max, 250);
    assert_eq!(bounds.refresh_timeout, Duration::from_millis(7_500));
    assert_eq!(bounds.max_inflight, ResolverBounds::default().max_inflight);

    for (var, bad) in [
        ("BUZZ_NIP_FI_OVERLAY_MAX", "0"),
        ("BUZZ_NIP_FI_SNAPSHOT_MAX_ROWS", "lots"),
        ("BUZZ_NIP_FI_RESOLVER_LOOKUP_TIMEOUT_MS", "600001"),
    ] {
        let prior = std::env::var(var).ok();
        std::env::set_var(var, bad);
        assert!(
            ResolverBounds::from_env().is_err(),
            "{var}={bad} must fail startup"
        );
        match prior {
            Some(p) => std::env::set_var(var, p),
            None => std::env::remove_var(var),
        }
    }
}

mod router;
