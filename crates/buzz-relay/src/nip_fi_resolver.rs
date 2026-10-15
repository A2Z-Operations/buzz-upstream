//! Database-sourced NIP-FI community resolution (`BUZZ_NIP_FI_COMMUNITY_SOURCE=database`).
//!
//! Each pod holds a snapshot of every registry-filtered binding of an active
//! community, loaded at startup and refreshed every 30s, start to start. A
//! community created through this pod after the published snapshot's read
//! sits in a bounded overlay, fenced by Postgres snapshot visibility, until a
//! refresh that sees it. Any other host goes to a bounded, single-flight
//! database lookup whose positive answer is never stored and whose "not
//! found" is cached for 5s.
//!
//! Every change to the snapshot pointer, the overlay, or the negative cache
//! happens under one publication lock and bumps a sequence number; reads of
//! the snapshot and overlay are lock-free. Admission fails closed with
//! `authorization_unavailable` once the last successful full load is older
//! than two minutes, re-checked after every await.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use arc_swap::{ArcSwap, ArcSwapOption};
use buzz_auth::{CommunityBinding, DenialClass, ProvisionBindingPolicy};
use buzz_core::tenant::normalize_host;
use buzz_db::{NipFiSnapshotLoad, PgSnapshot, RowCapExceeded};
use futures_util::future::{BoxFuture, FutureExt, Shared};
use tokio::sync::Semaphore;

use crate::config::ConfigError;

#[cfg(test)]
mod tests;

/// Refreshes start this often, measured start to start.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(30);
/// The last good snapshot serves for at most this long after its load.
pub const STALE_AFTER: Duration = Duration::from_secs(120);
/// How long a "not found" miss is cached.
pub const NEGATIVE_TTL: Duration = Duration::from_secs(5);

/// Configurable resolver bounds (plan §8). Every bound fails closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolverBounds {
    /// `BUZZ_NIP_FI_RESOLVER_NEGATIVE_CACHE_MAX`
    pub negative_cache_max: usize,
    /// `BUZZ_NIP_FI_RESOLVER_MAX_INFLIGHT`
    pub max_inflight: usize,
    /// `BUZZ_NIP_FI_RESOLVER_MAX_PENDING_HOSTS`
    pub max_pending_hosts: usize,
    /// `BUZZ_NIP_FI_RESOLVER_MAX_WAITERS`
    pub max_waiters: usize,
    /// `BUZZ_NIP_FI_RESOLVER_PERMIT_WAIT_MS`
    pub permit_wait: Duration,
    /// `BUZZ_NIP_FI_RESOLVER_LOOKUP_TIMEOUT_MS`
    pub lookup_timeout: Duration,
    /// `BUZZ_NIP_FI_SNAPSHOT_MAX_ROWS`
    pub snapshot_max_rows: usize,
    /// `BUZZ_NIP_FI_OVERLAY_MAX`
    pub overlay_max: usize,
    /// `BUZZ_NIP_FI_SNAPSHOT_REFRESH_TIMEOUT_MS`
    pub refresh_timeout: Duration,
}

impl Default for ResolverBounds {
    fn default() -> Self {
        Self {
            negative_cache_max: 100_000,
            max_inflight: 64,
            max_pending_hosts: 1_024,
            max_waiters: 4_096,
            permit_wait: Duration::from_millis(50),
            lookup_timeout: Duration::from_millis(500),
            snapshot_max_rows: 1_000_000,
            overlay_max: 10_000,
            refresh_timeout: Duration::from_millis(10_000),
        }
    }
}

impl ResolverBounds {
    /// Read every bound from the environment, defaulting unset ones.
    pub fn from_env() -> Result<Self, ConfigError> {
        let d = Self::default();
        let count = |name: &str, default: usize| -> Result<usize, ConfigError> {
            Ok(
                crate::nip_fi_config::parse_u64_bounded(name, 1, u32::MAX.into())?
                    .map_or(default, |v| v as usize),
            )
        };
        let millis = |name: &str, default: Duration| -> Result<Duration, ConfigError> {
            Ok(crate::nip_fi_config::parse_u64_bounded(name, 1, 600_000)?
                .map_or(default, Duration::from_millis))
        };
        let bounds = Self {
            negative_cache_max: count(
                "BUZZ_NIP_FI_RESOLVER_NEGATIVE_CACHE_MAX",
                d.negative_cache_max,
            )?,
            max_inflight: count("BUZZ_NIP_FI_RESOLVER_MAX_INFLIGHT", d.max_inflight)?,
            max_pending_hosts: count(
                "BUZZ_NIP_FI_RESOLVER_MAX_PENDING_HOSTS",
                d.max_pending_hosts,
            )?,
            max_waiters: count("BUZZ_NIP_FI_RESOLVER_MAX_WAITERS", d.max_waiters)?,
            permit_wait: millis("BUZZ_NIP_FI_RESOLVER_PERMIT_WAIT_MS", d.permit_wait)?,
            lookup_timeout: millis("BUZZ_NIP_FI_RESOLVER_LOOKUP_TIMEOUT_MS", d.lookup_timeout)?,
            snapshot_max_rows: count("BUZZ_NIP_FI_SNAPSHOT_MAX_ROWS", d.snapshot_max_rows)?,
            overlay_max: count("BUZZ_NIP_FI_OVERLAY_MAX", d.overlay_max)?,
            refresh_timeout: millis("BUZZ_NIP_FI_SNAPSHOT_REFRESH_TIMEOUT_MS", d.refresh_timeout)?,
        };
        Ok(bounds)
    }
}

/// Where bindings are read from. Implemented by [`buzz_db::Db`]; tests
/// substitute counting and failing sources.
pub trait BindingSource: Send + Sync + 'static {
    /// One consistent registry-filtered read of every active binding.
    /// `after_snapshot` runs once the read's snapshot is fixed, before its scan.
    fn load<'a>(
        &'a self,
        registry: &'a [String],
        max_rows: usize,
        after_snapshot: BoxFuture<'a, ()>,
    ) -> BoxFuture<'a, Result<Result<NipFiSnapshotLoad, RowCapExceeded>, String>>;

    /// The registry-filtered issuers of the active community at `host`.
    fn lookup<'a>(
        &'a self,
        host: &'a str,
        registry: &'a [String],
    ) -> BoxFuture<'a, Result<Result<Vec<String>, RowCapExceeded>, String>>;
}

impl BindingSource for buzz_db::Db {
    fn load<'a>(
        &'a self,
        registry: &'a [String],
        max_rows: usize,
        after_snapshot: BoxFuture<'a, ()>,
    ) -> BoxFuture<'a, Result<Result<NipFiSnapshotLoad, RowCapExceeded>, String>> {
        async move {
            self.load_nip_fi_binding_snapshot(registry, max_rows, after_snapshot)
                .await
                .map_err(|e| e.to_string())
        }
        .boxed()
    }

    fn lookup<'a>(
        &'a self,
        host: &'a str,
        registry: &'a [String],
    ) -> BoxFuture<'a, Result<Result<Vec<String>, RowCapExceeded>, String>> {
        async move {
            self.lookup_nip_fi_admission_binding(host, registry)
                .await
                .map_err(|e| e.to_string())
        }
        .boxed()
    }
}

/// Monotonic time since the resolver started; replaceable in tests.
pub trait Clock: Send + Sync + 'static {
    /// Elapsed time on this clock.
    fn now(&self) -> Duration;
}

struct TokioClock(tokio::time::Instant);

impl Clock for TokioClock {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
}

/// One published full load.
struct Published {
    map: HashMap<String, Arc<CommunityBinding>>,
    snapshot: PgSnapshot,
    /// Clock time the load started; the staleness deadline counts from here.
    loaded_at: Duration,
}

/// A community created through this pod, awaiting a refresh that sees it.
#[derive(Clone)]
struct OverlayEntry {
    binding: Arc<CommunityBinding>,
    xid: u64,
}

/// State changed only under the publication lock.
struct Writable {
    negative: lru::LruCache<String, Duration>,
}

type MissOutcome = Result<Option<Arc<CommunityBinding>>, ()>;
type PendingLookup = Shared<BoxFuture<'static, MissOutcome>>;

/// Why a refresh published nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadFailure {
    /// The store failed.
    Store(String),
    /// More rows than `BUZZ_NIP_FI_SNAPSHOT_MAX_ROWS`.
    OverCap,
    /// The whole attempt exceeded `BUZZ_NIP_FI_SNAPSHOT_REFRESH_TIMEOUT_MS`.
    Timeout,
    /// Its read started before the published one's (defensive).
    Superseded,
}

impl LoadFailure {
    fn label(&self) -> &'static str {
        match self {
            Self::Store(_) => "store",
            Self::OverCap => "over_cap",
            Self::Timeout => "timeout",
            Self::Superseded => "superseded",
        }
    }
}

/// The database-sourced community resolver.
pub struct DatabaseResolver {
    source: Arc<dyn BindingSource>,
    policy: ProvisionBindingPolicy,
    registry: Vec<String>,
    bounds: ResolverBounds,
    clock: Arc<dyn Clock>,
    published: ArcSwapOption<Published>,
    overlay: ArcSwap<HashMap<String, OverlayEntry>>,
    publication: Mutex<Writable>,
    sequence: AtomicU64,
    pending: Mutex<HashMap<String, PendingLookup>>,
    waiters: AtomicUsize,
    permits: Arc<Semaphore>,
    #[cfg(test)]
    pub(crate) hooks: hooks::Hooks,
}

impl DatabaseResolver {
    /// A resolver with no snapshot yet: every admission is unavailable until
    /// the first [`DatabaseResolver::refresh`] succeeds.
    pub fn new(
        source: Arc<dyn BindingSource>,
        policy: ProvisionBindingPolicy,
        registry: Vec<String>,
        bounds: ResolverBounds,
    ) -> Self {
        Self::with_clock(
            source,
            policy,
            registry,
            bounds,
            Arc::new(TokioClock(tokio::time::Instant::now())),
        )
    }

    pub(crate) fn with_clock(
        source: Arc<dyn BindingSource>,
        policy: ProvisionBindingPolicy,
        registry: Vec<String>,
        bounds: ResolverBounds,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let negative_cap =
            NonZeroUsize::new(bounds.negative_cache_max).unwrap_or(NonZeroUsize::MIN);
        Self {
            source,
            policy,
            registry,
            permits: Arc::new(Semaphore::new(bounds.max_inflight)),
            bounds,
            clock,
            published: ArcSwapOption::empty(),
            overlay: ArcSwap::default(),
            publication: Mutex::new(Writable {
                negative: lru::LruCache::new(negative_cap),
            }),
            sequence: AtomicU64::new(0),
            pending: Mutex::new(HashMap::new()),
            waiters: AtomicUsize::new(0),
            #[cfg(test)]
            hooks: hooks::Hooks::default(),
        }
    }

    /// Whether the startup load has succeeded (the readiness gate).
    pub fn is_loaded(&self) -> bool {
        self.published.load().is_some()
    }

    /// Resolve the community at `raw_host` for one admission.
    pub async fn resolve(
        self: &Arc<Self>,
        raw_host: &str,
    ) -> Result<Arc<CommunityBinding>, DenialClass> {
        const UNAVAILABLE: DenialClass = DenialClass::AuthorizationUnavailable;
        let host = normalize_host(raw_host);
        if !self.passes_prefilter(&host) {
            return Err(UNAVAILABLE);
        }
        self.ensure_fresh()?;
        if let Some(binding) = self.from_memory(&host) {
            return Ok(binding);
        }
        if self.negatively_cached(&host) {
            return Err(UNAVAILABLE);
        }
        let outcome = self.miss(host).await;
        self.ensure_fresh()?;
        match outcome {
            Ok(Some(binding)) => Ok(binding),
            Ok(None) | Err(()) => Err(UNAVAILABLE),
        }
    }

    /// The binding at `raw_host` from the snapshot or overlay only, for
    /// shadow metric labels. Never touches the database.
    pub fn resolve_from_memory(&self, raw_host: &str) -> Option<Arc<CommunityBinding>> {
        let host = normalize_host(raw_host);
        self.passes_prefilter(&host)
            .then(|| self.from_memory(&host))
            .flatten()
    }

    /// Record a binding this pod just committed in transaction `xid`, so
    /// login through this pod is immediate (plan §7, creation).
    pub async fn record_creation(&self, host: &str, issuers: &[String], xid: u64) {
        #[cfg(test)]
        self.hooks.hit(hooks::CREATION_BEFORE_OVERLAY).await;
        let host = normalize_host(host);
        if !self.passes_prefilter(&host) {
            return;
        }
        let issuers: Vec<String> = issuers
            .iter()
            .filter(|issuer| self.registry.contains(issuer))
            .cloned()
            .collect();
        let Some(binding) = self.binding_for(&host, issuers) else {
            return;
        };
        let mut writable = self.lock();
        if self
            .published
            .load()
            .as_ref()
            .is_some_and(|published| published.snapshot.is_visible(xid))
        {
            // That snapshot already reflects the creation or a later removal.
            return;
        }
        let mut overlay = HashMap::clone(&self.overlay.load());
        if overlay.len() >= self.bounds.overlay_max && !overlay.contains_key(&host) {
            metrics::counter!("buzz_nip_fi_overlay_full_total").increment(1);
            return;
        }
        overlay.insert(host.clone(), OverlayEntry { binding, xid });
        metrics::gauge!("buzz_nip_fi_overlay_size").set(overlay.len() as f64);
        self.overlay.store(Arc::new(overlay));
        writable.negative.pop(&host);
        self.bump(&writable);
    }

    /// Run one full load and publish it. A failure publishes nothing and
    /// leaves the staleness deadline where it was.
    pub async fn refresh(&self) -> Result<(), LoadFailure> {
        let started = self.clock.now();
        let timer = std::time::Instant::now();
        let attempt = async {
            let after_snapshot: BoxFuture<'_, ()> = {
                #[cfg(test)]
                {
                    self.hooks.hit(hooks::REFRESH_AFTER_SNAPSHOT).boxed()
                }
                #[cfg(not(test))]
                {
                    std::future::ready(()).boxed()
                }
            };
            let load = self
                .source
                .load(
                    &self.registry,
                    self.bounds.snapshot_max_rows,
                    after_snapshot,
                )
                .await
                .map_err(LoadFailure::Store)?
                .map_err(|_| LoadFailure::OverCap)?;
            let rows = load.rows.len();
            let map = self.build_map(load.rows);
            #[cfg(test)]
            self.hooks.hit(hooks::REFRESH_BEFORE_PUBLISH).await;
            self.publish(map, load.snapshot, started)?;
            Ok(rows)
        };
        let result = match tokio::time::timeout(self.bounds.refresh_timeout, attempt).await {
            Ok(result) => result,
            Err(_) => Err(LoadFailure::Timeout),
        };
        metrics::histogram!("buzz_nip_fi_snapshot_load_duration_seconds")
            .record(timer.elapsed().as_secs_f64());
        match result {
            Ok(rows) => {
                metrics::gauge!("buzz_nip_fi_snapshot_rows").set(rows as f64);
                self.record_age();
                Ok(())
            }
            Err(failure) => {
                tracing::warn!(
                    reason = failure.label(),
                    "NIP-FI binding snapshot load failed"
                );
                metrics::counter!("buzz_nip_fi_snapshot_load_failures_total", "reason" => failure.label())
                    .increment(1);
                self.record_age();
                Err(failure)
            }
        }
    }

    /// Refresh on a fixed schedule, start to start, never overlapping; a
    /// missed tick is skipped. The caller has already done the startup load.
    pub async fn run_refresh_loop(self: Arc<Self>, cancel: tokio_util::sync::CancellationToken) {
        let mut ticks = tokio::time::interval_at(
            tokio::time::Instant::now() + REFRESH_INTERVAL,
            REFRESH_INTERVAL,
        );
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                () = cancel.cancelled() => return,
                _ = ticks.tick() => {}
            }
            let _ = self.refresh().await;
        }
    }

    // ── Internals ───────────────────────────────────────────────────────────

    fn lock(&self) -> std::sync::MutexGuard<'_, Writable> {
        self.publication
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Bump the publication sequence; the caller holds the lock.
    fn bump(&self, _held: &Writable) {
        self.sequence.fetch_add(1, Ordering::AcqRel);
    }

    /// Canonical lowercase, exactly one label under the suffix, no port, not
    /// reserved. `host` is already normalized.
    fn passes_prefilter(&self, host: &str) -> bool {
        self.policy.is_suffix_host(host) && !self.policy.is_reserved(host)
    }

    fn ensure_fresh(&self) -> Result<(), DenialClass> {
        let fresh = self.published.load().as_ref().is_some_and(|published| {
            self.clock.now().saturating_sub(published.loaded_at) < STALE_AFTER
        });
        fresh
            .then_some(())
            .ok_or(DenialClass::AuthorizationUnavailable)
    }

    /// Snapshot, then overlay. Clones only the selected binding; each guard
    /// is dropped before returning.
    fn from_memory(&self, host: &str) -> Option<Arc<CommunityBinding>> {
        if let Some(binding) = self
            .published
            .load()
            .as_ref()
            .and_then(|published| published.map.get(host).cloned())
        {
            return Some(binding);
        }
        self.overlay
            .load()
            .get(host)
            .map(|entry| Arc::clone(&entry.binding))
    }

    fn negatively_cached(&self, host: &str) -> bool {
        let now = self.clock.now();
        let mut writable = self.lock();
        match writable.negative.get(host) {
            Some(expiry) if *expiry > now => true,
            Some(_) => {
                writable.negative.pop(host);
                self.bump(&writable);
                false
            }
            None => false,
        }
    }

    fn binding_for(&self, host: &str, issuers: Vec<String>) -> Option<Arc<CommunityBinding>> {
        CommunityBinding::new(format!("https://{host}"), issuers)
            .ok()
            .map(Arc::new)
    }

    fn build_map(&self, rows: Vec<(String, String)>) -> HashMap<String, Arc<CommunityBinding>> {
        let mut grouped: HashMap<String, Vec<String>> = HashMap::new();
        for (host, issuer) in rows {
            grouped
                .entry(normalize_host(&host))
                .or_default()
                .push(issuer);
        }
        grouped
            .into_iter()
            .filter_map(|(host, issuers)| {
                let binding = self.binding_for(&host, issuers)?;
                Some((host, binding))
            })
            .collect()
    }

    fn publish(
        &self,
        map: HashMap<String, Arc<CommunityBinding>>,
        snapshot: PgSnapshot,
        started: Duration,
    ) -> Result<(), LoadFailure> {
        let mut writable = self.lock();
        if self
            .published
            .load()
            .as_ref()
            .is_some_and(|published| started < published.loaded_at)
        {
            return Err(LoadFailure::Superseded);
        }
        let overlay: HashMap<String, OverlayEntry> = self
            .overlay
            .load()
            .iter()
            .filter(|(_, entry)| !snapshot.is_visible(entry.xid))
            .map(|(host, entry)| (host.clone(), entry.clone()))
            .collect();
        metrics::gauge!("buzz_nip_fi_overlay_size").set(overlay.len() as f64);
        self.overlay.store(Arc::new(overlay));
        self.published.store(Some(Arc::new(Published {
            map,
            snapshot,
            loaded_at: started,
        })));
        writable.negative.clear();
        metrics::gauge!("buzz_nip_fi_resolver_negative_cache_size").set(0.0);
        self.bump(&writable);
        Ok(())
    }

    fn record_age(&self) {
        if let Some(published) = self.published.load().as_ref() {
            metrics::gauge!("buzz_nip_fi_snapshot_age_seconds").set(
                self.clock
                    .now()
                    .saturating_sub(published.loaded_at)
                    .as_secs_f64(),
            );
        }
    }

    /// The bounded, single-flight miss lookup. A positive answer is returned
    /// to this caller only; a "not found" is cached only if no creation or
    /// swap happened while it ran.
    async fn miss(self: &Arc<Self>, host: String) -> MissOutcome {
        let lookup = {
            let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
            match pending.get(&host) {
                Some(lookup) => lookup.clone(),
                None => {
                    if pending.len() >= self.bounds.max_pending_hosts {
                        reject("pending_hosts");
                        return Err(());
                    }
                    let lookup = self.spawn_lookup(host.clone());
                    pending.insert(host, lookup.clone());
                    lookup
                }
            }
        };
        let Some(_waiter) = WaiterSlot::take(&self.waiters, self.bounds.max_waiters) else {
            reject("waiters");
            return Err(());
        };
        lookup.await
    }

    /// Run one lookup on its own task, so it completes (and leaves the
    /// pending map) even if every waiter goes away.
    fn spawn_lookup(self: &Arc<Self>, host: String) -> PendingLookup {
        let this = Arc::clone(self);
        let task = tokio::spawn(async move {
            let sequence = this.sequence.load(Ordering::Acquire);
            let outcome = this.lookup_once(&host).await;
            if matches!(outcome, Ok(None)) {
                this.store_negative(&host, sequence);
            }
            this.pending
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&host);
            outcome
        });
        async move { task.await.unwrap_or(Err(())) }
            .boxed()
            .shared()
    }

    async fn lookup_once(&self, host: &str) -> MissOutcome {
        let permit = match tokio::time::timeout(
            self.bounds.permit_wait,
            Arc::clone(&self.permits).acquire_owned(),
        )
        .await
        {
            Ok(Ok(permit)) => permit,
            _ => {
                reject("permit");
                return Err(());
            }
        };
        let timer = std::time::Instant::now();
        let read = tokio::time::timeout(
            self.bounds.lookup_timeout,
            self.source.lookup(host, &self.registry),
        )
        .await;
        drop(permit);
        metrics::histogram!("buzz_nip_fi_resolver_lookup_duration_seconds")
            .record(timer.elapsed().as_secs_f64());
        #[cfg(test)]
        self.hooks.hit(hooks::MISS_AFTER_READ).await;
        match read {
            Err(_) => {
                metrics::counter!("buzz_nip_fi_resolver_lookup_timeouts_total").increment(1);
                Err(())
            }
            Ok(Err(_)) | Ok(Ok(Err(RowCapExceeded(_)))) => Err(()),
            Ok(Ok(Ok(issuers))) if issuers.is_empty() => Ok(None),
            Ok(Ok(Ok(issuers))) => Ok(self.binding_for(host, issuers)),
        }
    }

    fn store_negative(&self, host: &str, sequence: u64) {
        let mut writable = self.lock();
        if self.sequence.load(Ordering::Acquire) != sequence {
            return;
        }
        let expiry = self.clock.now() + NEGATIVE_TTL;
        if let Some((evicted, _)) = writable.negative.push(host.to_owned(), expiry) {
            if evicted != host {
                metrics::counter!("buzz_nip_fi_resolver_negative_evictions_total").increment(1);
            }
        }
        metrics::gauge!("buzz_nip_fi_resolver_negative_cache_size")
            .set(writable.negative.len() as f64);
        self.bump(&writable);
    }

    #[cfg(test)]
    pub(crate) fn overlay_len(&self) -> usize {
        self.overlay.load().len()
    }

    #[cfg(test)]
    pub(crate) fn negative_len(&self) -> usize {
        self.lock().negative.len()
    }

    #[cfg(test)]
    pub(crate) fn pending_len(&self) -> usize {
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    #[cfg(test)]
    pub(crate) fn waiters_now(&self) -> usize {
        self.waiters.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn inflight_now(&self) -> usize {
        self.bounds.max_inflight - self.permits.available_permits()
    }
}

fn reject(reason: &'static str) {
    metrics::counter!("buzz_nip_fi_resolver_miss_rejections_total", "reason" => reason)
        .increment(1);
}

/// One counted caller waiting on a pending lookup.
struct WaiterSlot<'a>(&'a AtomicUsize);

impl<'a> WaiterSlot<'a> {
    fn take(waiters: &'a AtomicUsize, max: usize) -> Option<Self> {
        waiters
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < max).then_some(n + 1)
            })
            .ok()
            .map(|_| Self(waiters))
    }
}

impl Drop for WaiterSlot<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
pub(crate) mod hooks {
    //! Per-resolver pause points for ordering tests.
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use tokio::sync::Semaphore;

    pub(crate) const REFRESH_AFTER_SNAPSHOT: &str = "refresh.after_snapshot";
    pub(crate) const REFRESH_BEFORE_PUBLISH: &str = "refresh.before_publish";
    pub(crate) const MISS_AFTER_READ: &str = "miss.after_read";
    pub(crate) const CREATION_BEFORE_OVERLAY: &str = "creation.before_overlay";

    /// An armed pause: production signals `reached`, then waits for `release`.
    pub(crate) struct Gate {
        reached: Semaphore,
        release: Semaphore,
    }

    impl Gate {
        pub(crate) async fn reached(&self) {
            self.reached.acquire().await.expect("open").forget();
        }

        pub(crate) fn release(&self) {
            self.release.add_permits(1);
        }
    }

    #[derive(Default)]
    pub(crate) struct Hooks(Mutex<HashMap<&'static str, Arc<Gate>>>);

    impl Hooks {
        /// Pause every later hit of `point` until released, once per release.
        pub(crate) fn arm(&self, point: &'static str) -> Arc<Gate> {
            let gate = Arc::new(Gate {
                reached: Semaphore::new(0),
                release: Semaphore::new(0),
            });
            self.0.lock().unwrap().insert(point, Arc::clone(&gate));
            gate
        }

        pub(crate) fn disarm(&self, point: &'static str) {
            self.0.lock().unwrap().remove(point);
        }

        pub(crate) async fn hit(&self, point: &'static str) {
            let gate = self.0.lock().unwrap().get(point).cloned();
            if let Some(gate) = gate {
                gate.reached.add_permits(1);
                gate.release.acquire().await.expect("open").forget();
            }
        }
    }
}
