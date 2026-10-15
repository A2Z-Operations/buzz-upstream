//! Stdout access log for security investigations: who read what, and who
//! tried to authenticate.
//!
//! Every line goes to the [`TARGET`] tracing target (stdout, never the
//! `audit_log` table) and carries an `event` field to filter on. Search text,
//! event content, tokens, and assertions are never recorded: filters are
//! reduced to a [`FilterSummary`], and failures to fixed reason codes.

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use buzz_core::CommunityId;
use nostr::{Alphabet, Filter, PublicKey, SingleLetterTag};
use tracing::Level;
use uuid::Uuid;

/// The tracing target for every access line.
pub(crate) const TARGET: &str = "buzz_relay::access";

/// Most identifiers listed per field; the matching `*_count` field is exact.
const LIST_CAP: usize = 10;

macro_rules! emit {
    ($level:expr, $($fields:tt)+) => {
        if $level == Level::WARN {
            tracing::warn!(target: TARGET, $($fields)+)
        } else {
            tracing::info!(target: TARGET, $($fields)+)
        }
    };
}

/// What a REQ or COUNT asked for, merged across its filters. Kinds and
/// identifiers are de-duplicated unions; `since`/`until`/`limit` take the
/// widest bound and are absent when any filter leaves them open. Only
/// well-formed channel ids and pubkeys are listed, so client-chosen tag text
/// never reaches the log; the counts include every distinct value.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct FilterSummary {
    filters: usize,
    kinds: Option<String>,
    channel_ids: Option<String>,
    channel_count: usize,
    authors: Option<String>,
    author_count: usize,
    p_tags: Option<String>,
    p_count: usize,
    ids: usize,
    since: Option<u64>,
    until: Option<u64>,
    limit: Option<usize>,
    search: bool,
}

impl FilterSummary {
    pub(crate) fn new(filters: &[Filter]) -> Self {
        let h = SingleLetterTag::lowercase(Alphabet::H);
        let p = SingleLetterTag::lowercase(Alphabet::P);
        let mut kinds = BTreeSet::new();
        let mut channels = BTreeSet::new();
        let mut authors = BTreeSet::new();
        let mut p_tags = BTreeSet::new();
        let mut ids: BTreeSet<&nostr::EventId> = BTreeSet::new();
        for filter in filters {
            kinds.extend(filter.kinds.iter().flatten().map(|k| k.as_u16()));
            channels.extend(filter.generic_tags.get(&h).into_iter().flatten());
            authors.extend(filter.authors.iter().flatten().map(PublicKey::to_hex));
            p_tags.extend(filter.generic_tags.get(&p).into_iter().flatten());
            ids.extend(filter.ids.iter().flatten());
        }
        let bound = |pick: fn(&Filter) -> Option<u64>, widest: fn(u64, u64) -> u64| {
            let values: Option<Vec<u64>> = filters.iter().map(pick).collect();
            values?.into_iter().reduce(widest)
        };
        Self {
            filters: filters.len(),
            kinds: join(kinds.iter().map(u16::to_string)),
            channel_ids: join(channels.iter().filter(|c| Uuid::parse_str(c).is_ok())),
            channel_count: channels.len(),
            authors: join(authors.iter()),
            author_count: authors.len(),
            p_tags: join(p_tags.iter().filter(|v| PublicKey::from_hex(v).is_ok())),
            p_count: p_tags.len(),
            ids: ids.len(),
            since: bound(|f| f.since.map(|t| t.as_secs()), u64::min),
            until: bound(|f| f.until.map(|t| t.as_secs()), u64::max),
            limit: bound(|f| f.limit.map(|l| l as u64), u64::max).map(|l| l as usize),
            search: filters.iter().any(|f| f.search.is_some()),
        }
    }
}

/// Comma-join at most [`LIST_CAP`] values; `None` when there are none.
fn join<T: ToString>(values: impl Iterator<Item = T>) -> Option<String> {
    let listed: Vec<String> = values.take(LIST_CAP).map(|v| v.to_string()).collect();
    (!listed.is_empty()).then(|| listed.join(","))
}

/// How a read ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReadOutcome {
    /// Dropped before a terminal frame: connection teardown or session expiry.
    Aborted,
    Served,
    Denied(&'static str),
    Error(&'static str),
}

/// One access line per WebSocket REQ or COUNT, written when the handler
/// finishes, including when its future is dropped mid-read.
pub(crate) struct ReadLog {
    event: &'static str,
    conn_id: Uuid,
    community: CommunityId,
    pubkey: Option<String>,
    sub_id: String,
    summary: FilterSummary,
    outcome: ReadOutcome,
    results: usize,
}

impl ReadLog {
    pub(crate) fn req(
        conn: &crate::connection::ConnectionState,
        sub_id: &str,
        filters: &[Filter],
    ) -> Self {
        Self::new("req", conn, sub_id, filters)
    }

    pub(crate) fn count(
        conn: &crate::connection::ConnectionState,
        sub_id: &str,
        filters: &[Filter],
    ) -> Self {
        Self::new("count", conn, sub_id, filters)
    }

    fn new(
        event: &'static str,
        conn: &crate::connection::ConnectionState,
        sub_id: &str,
        filters: &[Filter],
    ) -> Self {
        let pubkey = match conn.auth_state_snapshot() {
            crate::connection::AuthState::Authenticated(ctx) => Some(ctx.pubkey.to_hex()),
            _ => None,
        };
        Self {
            event,
            conn_id: conn.conn_id,
            community: conn.tenant.community(),
            pubkey,
            sub_id: sub_id.to_owned(),
            summary: FilterSummary::new(filters),
            outcome: ReadOutcome::Aborted,
            results: 0,
        }
    }

    /// Refused before any stored event was read.
    pub(crate) fn deny(&mut self, reason: &'static str) {
        self.outcome = ReadOutcome::Denied(reason);
    }

    /// Ended by a server-side failure.
    pub(crate) fn error(&mut self, reason: &'static str) {
        self.outcome = ReadOutcome::Error(reason);
    }

    /// Completed: `results` stored events sent before EOSE, or the COUNT total.
    pub(crate) fn served(&mut self, results: usize) {
        self.outcome = ReadOutcome::Served;
        self.results = results;
    }
}

impl Drop for ReadLog {
    fn drop(&mut self) {
        let (outcome, reason) = match self.outcome {
            ReadOutcome::Aborted => ("aborted", None),
            ReadOutcome::Served => ("served", None),
            ReadOutcome::Denied(r) => ("denied", Some(r)),
            ReadOutcome::Error(r) => ("error", Some(r)),
        };
        let s = &self.summary;
        let (stored_events, count) = match self.event {
            "count" => (None, Some(self.results)),
            _ => (Some(self.results), None),
        };
        emit!(
            Level::INFO,
            event = self.event,
            conn_id = %self.conn_id,
            community = %self.community,
            pubkey = self.pubkey.as_deref(),
            sub_id = self.sub_id.as_str(),
            filters = s.filters,
            kinds = s.kinds.as_deref(),
            channel_ids = s.channel_ids.as_deref(),
            channel_count = s.channel_count,
            authors = s.authors.as_deref(),
            author_count = s.author_count,
            p_tags = s.p_tags.as_deref(),
            p_count = s.p_count,
            ids = s.ids,
            since = s.since,
            until = s.until,
            limit = s.limit,
            search = s.search,
            outcome,
            reason,
            stored_events,
            count,
            "access"
        );
    }
}

/// Which credential an auth attempt presented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AuthMethod {
    /// WebSocket login (NIP-42).
    Nip42,
    /// HTTP signed request (NIP-98).
    Nip98,
    /// Enterprise identity assertion (NIP-FI).
    NipFi,
}

impl AuthMethod {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Nip42 => "nip42",
            Self::Nip98 => "nip98",
            Self::NipFi => "nip_fi",
        }
    }
}

/// How an auth attempt ended. `Denied` means the identity was proven but not
/// admitted; `Failed` means the proof itself was rejected. Reasons are fixed
/// codes, never raw error text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AuthResult {
    Success,
    Denied(&'static str),
    Failed(&'static str),
}

impl AuthResult {
    /// The access result for a WebSocket auth terminal. Only a bad proof is
    /// `Failed`; every policy refusal, including a fail-closed lookup error,
    /// is `Denied` with the metric's stable label as its reason.
    pub(crate) fn from_outcome(outcome: crate::metrics::AuthOutcome) -> Self {
        use crate::metrics::AuthOutcome as O;
        match outcome {
            O::Success => Self::Success,
            O::Invalid => Self::Failed(outcome.as_str()),
            _ => Self::Denied(outcome.as_str()),
        }
    }
}

/// A fixed code for a rejected proof. `Nip98Invalid` and `Internal` carry
/// free text that can echo request data, so they collapse to one code each.
pub(crate) fn auth_error_code(e: &buzz_auth::AuthError) -> &'static str {
    use buzz_auth::AuthError as E;
    match e {
        E::InvalidSignature => "invalid_signature",
        E::ChallengeMismatch => "challenge_mismatch",
        E::RelayUrlMismatch => "relay_url_mismatch",
        E::EventExpired => "event_expired",
        E::Nip98Invalid(_) => "nip98_invalid",
        E::Nip98Replay => "nip98_replay",
        E::PubkeyMismatch => "pubkey_mismatch",
        E::InsufficientScope { .. } => "insufficient_scope",
        E::ChannelAccessDenied => "channel_access_denied",
        E::Internal(_) => "internal",
    }
}

/// One auth attempt. `pubkey_verified` is false when the key is only what
/// the client claimed (a failed signature, or a key never checked).
pub(crate) struct AuthAttempt<'a> {
    pub(crate) method: AuthMethod,
    pub(crate) community: Option<CommunityId>,
    pub(crate) conn_id: Option<Uuid>,
    pub(crate) route: Option<&'a str>,
    pub(crate) pubkey: Option<PublicKey>,
    pub(crate) pubkey_verified: bool,
}

impl AuthAttempt<'_> {
    /// A WebSocket login on `conn` by `pubkey`.
    pub(crate) fn nip42(
        conn: &crate::connection::ConnectionState,
        pubkey: PublicKey,
        pubkey_verified: bool,
    ) -> Self {
        Self {
            method: AuthMethod::Nip42,
            community: Some(conn.tenant.community()),
            conn_id: Some(conn.conn_id),
            route: None,
            pubkey: Some(pubkey),
            pubkey_verified,
        }
    }

    /// An enterprise-assertion admission check. It runs before a community
    /// id is bound, so the line carries none.
    pub(crate) fn nip_fi(pubkey: Option<PublicKey>, pubkey_verified: bool) -> Self {
        Self {
            method: AuthMethod::NipFi,
            community: None,
            conn_id: None,
            route: None,
            pubkey,
            pubkey_verified,
        }
    }

    /// Finish the WebSocket login with `outcome`, logging it only if this
    /// call won the connection's single auth terminal.
    pub(crate) fn reject(
        &self,
        conn: &crate::connection::ConnectionState,
        outcome: crate::metrics::AuthOutcome,
    ) -> bool {
        let won = conn.reject_auth(outcome);
        if won {
            self.log(AuthResult::from_outcome(outcome));
        }
        won
    }

    pub(crate) fn log(&self, result: AuthResult) {
        let (level, outcome, reason) = match result {
            AuthResult::Success => (Level::INFO, "success", None),
            AuthResult::Denied(r) => (Level::WARN, "denied", Some(r)),
            AuthResult::Failed(r) => (Level::WARN, "failed", Some(r)),
        };
        let pubkey = self.pubkey.as_ref().map(PublicKey::to_hex);
        emit!(
            level,
            event = "auth",
            method = self.method.as_str(),
            conn_id = self.conn_id.map(tracing::field::display),
            community = self.community.map(tracing::field::display),
            route = self.route,
            pubkey = pubkey.as_deref(),
            pubkey_verified = pubkey.is_some().then_some(self.pubkey_verified),
            outcome,
            reason,
            "access"
        );
    }
}

/// Frames written to one WebSocket: stored events and live pushes alike.
#[derive(Debug, Default)]
pub(crate) struct SentTotals {
    events: AtomicU64,
    bytes: AtomicU64,
}

impl SentTotals {
    /// Count a frame as it is handed to the socket.
    pub(crate) fn record(&self, frame: &axum::extract::ws::Message) {
        use axum::extract::ws::Message;
        let len = match frame {
            Message::Text(text) => {
                if text.as_str().starts_with("[\"EVENT\"") {
                    self.events.fetch_add(1, Ordering::Relaxed);
                }
                text.len()
            }
            Message::Binary(bytes) => bytes.len(),
            _ => return,
        };
        self.bytes.fetch_add(len as u64, Ordering::Relaxed);
    }
}

/// The connection opened.
pub(crate) fn log_connect(conn_id: Uuid, community: CommunityId, peer_addr: SocketAddr) {
    emit!(
        Level::INFO,
        event = "connect",
        conn_id = %conn_id,
        community = %community,
        peer_addr = %peer_addr,
        "access"
    );
}

/// The connection closed. `pubkey` is the identity it authenticated as, if any.
pub(crate) fn log_close(
    conn_id: Uuid,
    community: CommunityId,
    peer_addr: SocketAddr,
    pubkey: Option<&PublicKey>,
    duration: Duration,
    sent: &SentTotals,
) {
    let pubkey = pubkey.map(PublicKey::to_hex);
    emit!(
        Level::INFO,
        event = "close",
        conn_id = %conn_id,
        community = %community,
        peer_addr = %peer_addr,
        pubkey = pubkey.as_deref(),
        duration_ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
        events_sent = sent.events.load(Ordering::Relaxed),
        bytes_sent = sent.bytes.load(Ordering::Relaxed),
        "access"
    );
}

/// A fixed description of a driver error for log lines. Postgres primary
/// messages can repeat a bound value (`invalid input syntax for type uuid:
/// "<value>"`), so only the SQLSTATE code is kept.
pub(crate) fn sqlx_error_kind(e: &sqlx::Error) -> String {
    let kind = match e {
        sqlx::Error::Database(db) => {
            return format!("database:{}", db.code().as_deref().unwrap_or("unknown"))
        }
        sqlx::Error::RowNotFound => "row_not_found",
        sqlx::Error::PoolTimedOut => "pool_timed_out",
        sqlx::Error::PoolClosed => "pool_closed",
        sqlx::Error::Io(_) => "io",
        sqlx::Error::Tls(_) => "tls",
        sqlx::Error::Protocol(_) => "protocol",
        sqlx::Error::ColumnDecode { .. } | sqlx::Error::Decode(_) => "decode",
        sqlx::Error::Encode(_) => "encode",
        _ => "other",
    };
    kind.to_owned()
}

/// [`sqlx_error_kind`] for the relay's database error type.
pub(crate) fn db_error_kind(e: &buzz_db::DbError) -> String {
    use buzz_db::DbError as E;
    let kind = match e {
        E::Sqlx(e) => return sqlx_error_kind(e),
        E::Serde(_) => "serde",
        E::InvalidData(_) => "invalid_data",
        E::NotFound(_) | E::ChannelNotFound(_) | E::MemberNotFound(_) => "not_found",
        E::AccessDenied(_) => "access_denied",
        _ => "other",
    };
    kind.to_owned()
}

/// The path of a request URL, without scheme, host, or query string. The
/// query is dropped because it can carry client-chosen text.
pub(crate) fn url_path(url: &str) -> &str {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let path = after_scheme.find('/').map_or("/", |i| &after_scheme[i..]);
    path.split(['?', '#']).next().unwrap_or(path)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::prelude::*;

    #[derive(Clone, Default)]
    pub(crate) struct Captured(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Captured {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Captured {
        /// Access lines written so far, parsed from the production JSON shape.
        pub(crate) fn lines(&self) -> Vec<serde_json::Value> {
            String::from_utf8(self.0.lock().unwrap().clone())
                .unwrap()
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .filter(|v: &serde_json::Value| v["target"] == TARGET)
                .collect()
        }
    }

    /// Run `f` with JSON logs (flattened, as in production) captured.
    pub(crate) fn capture<R>(f: impl FnOnce() -> R) -> (R, Captured) {
        let _lock = crate::test_support::tracing_dispatch_lock();
        let out = Captured::default();
        let writer = out.clone();
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .json()
                .flatten_event(true)
                .with_writer(move || writer.clone()),
        );
        let r = tracing::subscriber::with_default(subscriber, f);
        (r, out)
    }

    fn filter(json: serde_json::Value) -> Filter {
        serde_json::from_value(json).unwrap()
    }

    const PK_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn pk() -> String {
        nostr::Keys::generate().public_key().to_hex()
    }

    #[test]
    fn summary_merges_filters_to_the_widest_bounds() {
        let s = FilterSummary::new(&[
            filter(serde_json::json!({"kinds": [9, 7], "since": 100, "until": 200, "limit": 10})),
            filter(serde_json::json!({"kinds": [7, 5], "since": 50, "until": 300, "limit": 50})),
        ]);
        assert_eq!(s.filters, 2);
        assert_eq!(s.kinds.as_deref(), Some("5,7,9"));
        assert_eq!((s.since, s.until, s.limit), (Some(50), Some(300), Some(50)));
        assert!(!s.search);
    }

    #[test]
    fn summary_omits_a_bound_any_filter_leaves_open() {
        let s = FilterSummary::new(&[
            filter(serde_json::json!({"since": 100, "limit": 10})),
            filter(serde_json::json!({"until": 300})),
        ]);
        assert_eq!((s.since, s.until, s.limit), (None, None, None));
    }

    #[test]
    fn summary_caps_lists_and_counts_every_distinct_value() {
        let channels: Vec<String> = (0..14).map(|_| Uuid::new_v4().to_string()).collect();
        let ps: Vec<String> = (0..12).map(|_| pk()).collect();
        let s = FilterSummary::new(&[filter(serde_json::json!({
            "#h": channels, "#p": ps, "authors": [PK_A], "ids": [PK_A]
        }))]);
        assert_eq!(s.channel_count, 14);
        assert_eq!(s.channel_ids.unwrap().split(',').count(), LIST_CAP);
        assert_eq!(s.p_count, 12);
        assert_eq!(s.p_tags.unwrap().split(',').count(), LIST_CAP);
        assert_eq!(
            (s.authors.as_deref(), s.author_count, s.ids),
            (Some(PK_A), 1, 1)
        );
    }

    #[test]
    fn summary_never_lists_malformed_tag_text() {
        let s = FilterSummary::new(&[filter(serde_json::json!({
            "#h": ["secret plans for tuesday"], "#p": ["not a pubkey"]
        }))]);
        assert_eq!((s.channel_ids, s.channel_count), (None, 1));
        assert_eq!((s.p_tags, s.p_count), (None, 1));
    }

    #[test]
    fn req_line_records_search_use_but_never_the_search_text() {
        let needle = "zebra-crossing-7f3a";
        let filters = [filter(serde_json::json!({"kinds": [9], "search": needle}))];
        let ((), out) = capture(|| {
            let mut log = ReadLog {
                event: "req",
                conn_id: Uuid::nil(),
                community: CommunityId::from_uuid(Uuid::nil()),
                pubkey: Some(PK_A.to_owned()),
                sub_id: "sub".to_owned(),
                summary: FilterSummary::new(&filters),
                outcome: ReadOutcome::Aborted,
                results: 0,
            };
            log.served(3);
        });
        let raw = String::from_utf8(out.0.lock().unwrap().clone()).unwrap();
        assert!(!raw.contains(needle), "search text leaked: {raw}");
        let lines = out.lines();
        assert_eq!(lines.len(), 1);
        let line = &lines[0];
        assert_eq!(line["event"], "req");
        assert_eq!(line["search"], true);
        assert_eq!(line["outcome"], "served");
        assert_eq!(line["stored_events"], 3);
        assert_eq!(line["pubkey"], PK_A);
        assert_eq!(line["kinds"], "9");
        assert!(line.get("since").is_none() && line.get("reason").is_none());
        assert!(line.get("count").is_none());
    }

    #[test]
    fn dropped_read_logs_as_aborted() {
        let ((), out) = capture(|| {
            let _log = ReadLog {
                event: "count",
                conn_id: Uuid::nil(),
                community: CommunityId::from_uuid(Uuid::nil()),
                pubkey: None,
                sub_id: "c".to_owned(),
                summary: FilterSummary::default(),
                outcome: ReadOutcome::Aborted,
                results: 0,
            };
        });
        let line = &out.lines()[0];
        assert_eq!(
            (&line["event"], &line["outcome"]),
            (&"count".into(), &"aborted".into())
        );
        assert_eq!(line["count"], 0);
        assert!(line.get("pubkey").is_none());
    }

    #[test]
    fn auth_failure_is_warn_and_marks_the_claimed_key_unverified() {
        let key = nostr::Keys::generate().public_key();
        let ((), out) = capture(|| {
            AuthAttempt {
                method: AuthMethod::Nip98,
                community: None,
                conn_id: None,
                route: Some("/query"),
                pubkey: Some(key),
                pubkey_verified: false,
            }
            .log(AuthResult::Failed("invalid_signature"));
        });
        let line = &out.lines()[0];
        assert_eq!(line["level"], "WARN");
        assert_eq!(line["event"], "auth");
        assert_eq!(line["method"], "nip98");
        assert_eq!(line["outcome"], "failed");
        assert_eq!(line["reason"], "invalid_signature");
        assert_eq!(line["pubkey"], key.to_hex());
        assert_eq!(line["pubkey_verified"], false);
        assert_eq!(line["route"], "/query");
    }

    #[test]
    fn auth_success_is_info_without_a_reason() {
        let ((), out) = capture(|| {
            AuthAttempt {
                method: AuthMethod::Nip42,
                community: Some(CommunityId::from_uuid(Uuid::nil())),
                conn_id: Some(Uuid::nil()),
                route: None,
                pubkey: Some(nostr::Keys::generate().public_key()),
                pubkey_verified: true,
            }
            .log(AuthResult::Success);
        });
        let line = &out.lines()[0];
        assert_eq!(
            (&line["level"], &line["outcome"]),
            (&"INFO".into(), &"success".into())
        );
        assert_eq!(line["pubkey_verified"], true);
        assert!(line.get("reason").is_none() && line.get("route").is_none());
    }

    #[test]
    fn url_path_drops_host_and_query() {
        assert_eq!(url_path("https://relay.example/query?search=hi"), "/query");
        assert_eq!(url_path("https://relay.example"), "/");
        assert_eq!(url_path("/events#x"), "/events");
    }
}
