//! End-to-end tests for kind:30180 owner records (NIP-AP).
//!
//! An owner record holds an owner's plaintext settings for one agent
//! (`d` = `agent:<pubkey>`) or one team (`d` = `team:<team-id>`). These tests
//! assert the relay contract from NIP-AP "Ingest validation: kind:30180" and
//! "Access control: kind:30180 author-only":
//! - ingest validation of the `d` tag and the `h` ban, with `invalid:`;
//! - parameterized-replaceable head selection on `(pubkey, 30180, d)`;
//! - author-only reads on REQ, `ids` lookup, live fan-out, WS and HTTP COUNT,
//!   and the HTTP bridge query, with the owner as the positive control;
//! - exclusion from full-text search, for the author too;
//! - NIP-09 deletions: the `k` tag is required for every coordinate or id
//!   that names a record, the deletion is as private as the record whether or
//!   not its target is stored, and a non-author cannot delete a record.
//!
//! # Running
//!
//! Start the relay, then run:
//!
//! ```text
//! RELAY_URL=ws://localhost:3000 cargo test -p buzz-test-client --test e2e_owner_records -- --ignored
//! ```

use std::collections::HashSet;
use std::time::Duration;

use buzz_test_client::{BuzzTestClient, RelayMessage};
use nostr::{EventBuilder, Filter, Keys, Kind, Tag, Timestamp};
use serde_json::{json, Value};

#[path = "support/author_only_deletion.rs"]
mod aod;

const DEFAULT_RELAY_URL: &str = "ws://localhost:3000";
const OWNER_RECORD_KIND: u16 = 30180;

use aod::{relay_http_url, relay_url};

fn sub_id(name: &str) -> String {
    format!("e2e-owner-record-{name}-{}", uuid::Uuid::new_v4())
}

fn record_event(keys: &Keys, tags: Vec<Vec<&str>>, content: &str, created_at: u64) -> nostr::Event {
    EventBuilder::new(Kind::Custom(OWNER_RECORD_KIND), content)
        .tags(tags.into_iter().map(|t| Tag::parse(t).unwrap()))
        .custom_created_at(Timestamp::from(created_at))
        .sign_with_keys(keys)
        .unwrap()
}

fn record(keys: &Keys, d: &str, content: &str, created_at: u64) -> nostr::Event {
    record_event(keys, vec![vec!["d", d]], content, created_at)
}

fn agent_d() -> String {
    format!("agent:{}", Keys::generate().public_key().to_hex())
}

fn team_d() -> String {
    format!("team:{}", uuid::Uuid::new_v4())
}

fn coordinate(owner: &Keys, d: &str) -> String {
    format!("{OWNER_RECORD_KIND}:{}:{d}", owner.public_key().to_hex())
}

fn now() -> u64 {
    Timestamp::now().as_secs()
}

async fn publish(client: &mut BuzzTestClient, event: nostr::Event) {
    let ok = client.send_event(event).await.expect("send record");
    assert!(ok.accepted, "relay rejected a valid record: {}", ok.message);
}

async fn req(client: &mut BuzzTestClient, name: &str, filter: Value) -> Vec<nostr::Event> {
    let sid = sub_id(name);
    client
        .send_raw(&json!(["REQ", sid, filter]))
        .await
        .expect("send REQ");
    let events = client
        .collect_until_eose(&sid, Duration::from_secs(5))
        .await
        .expect("collect");
    client.close_subscription(&sid).await.expect("close");
    events
}

async fn ws_count(client: &mut BuzzTestClient, filter: Value) -> u64 {
    let sid = sub_id("count");
    client
        .send_raw(&json!(["COUNT", sid, filter]))
        .await
        .expect("send COUNT");
    loop {
        match client
            .recv_event(Duration::from_secs(5))
            .await
            .expect("recv")
        {
            RelayMessage::Count {
                subscription_id,
                count,
            } if subscription_id == sid => return count,
            RelayMessage::Closed {
                subscription_id,
                message,
            } if subscription_id == sid => panic!("COUNT closed: {message}"),
            _ => {}
        }
    }
}

async fn http_post(keys: &Keys, path: &str, filters: Value) -> (u16, Value) {
    let resp = reqwest::Client::new()
        .post(format!("{}{path}", relay_http_url()))
        .header("X-Pubkey", keys.public_key().to_hex())
        .json(&filters)
        .send()
        .await
        .expect("http request");
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(Value::Null))
}

fn ids(events: &[nostr::Event]) -> Vec<String> {
    events.iter().map(|e| e.id.to_hex()).collect()
}

/// Every envelope rule rejects with `invalid:`. Tags are counted by first
/// element, and the `d` value is compared byte for byte.
#[tokio::test]
#[ignore]
async fn test_owner_record_rejects_malformed_envelopes() {
    let keys = Keys::generate();
    let hex = Keys::generate().public_key().to_hex();
    let agent = format!("agent:{hex}");
    let upper = format!("agent:{}", hex.to_uppercase());
    let short = format!("agent:{}", &hex[..63]);
    let long_team = format!("team:{}", "t".repeat(65));
    let channel = uuid::Uuid::new_v4().to_string();
    let cases: Vec<(&str, Vec<Vec<&str>>)> = vec![
        ("no d", vec![]),
        ("valueless d", vec![vec!["d"]]),
        (
            "valueless d plus valid d",
            vec![vec!["d"], vec!["d", &agent]],
        ),
        ("two d", vec![vec!["d", &agent], vec!["d", "team:t"]]),
        ("bare pubkey", vec![vec!["d", &hex]]),
        ("uppercase prefix", vec![vec!["d", "Team:t"]]),
        ("uppercase hex", vec![vec!["d", &upper]]),
        ("short hex", vec![vec!["d", &short]]),
        ("empty team", vec![vec!["d", "team:"]]),
        ("whitespace team", vec![vec!["d", "team:a b"]]),
        ("long team", vec![vec!["d", &long_team]]),
        ("h tag", vec![vec!["d", &agent], vec!["h", &channel]]),
    ];

    let mut client = BuzzTestClient::connect(&relay_url(), &keys)
        .await
        .expect("connect");
    for (name, tags) in cases {
        let ok = client
            .send_event(record_event(&keys, tags, "{}", now()))
            .await
            .expect("send record");
        assert!(!ok.accepted, "{name}: must be rejected");
        assert!(
            ok.message.starts_with("invalid:"),
            "{name}: expected an `invalid:` refusal, got: {}",
            ok.message
        );
    }
    client.disconnect().await.expect("disconnect");
}

/// Agent and team records are accepted with unvalidated content, and each
/// `(pubkey, 30180, d)` address keeps only its newest record.
#[tokio::test]
#[ignore]
async fn test_owner_record_head_selection_per_address() {
    let owner = Keys::generate();
    let author = owner.public_key().to_hex();
    let (agent, team) = (agent_d(), "team:builtin-team:Welcome".to_string());
    let base = now() - 100;
    let mut ws = BuzzTestClient::connect(&relay_url(), &owner)
        .await
        .expect("connect");
    let agent_new = record(&owner, &agent, "not json", base + 1);
    let team_new = record(&owner, &team, r#"{"name":"T","members":[]}"#, base + 1);
    for e in [
        record(&owner, &agent, "{}", base),
        agent_new.clone(),
        record(&owner, &team, "{}", base),
        team_new.clone(),
    ] {
        publish(&mut ws, e).await;
    }
    let stale = record(&owner, &agent, "{}", base - 1);
    let _ = ws.send_event(stale).await.expect("send stale");

    for (d, head) in [(&agent, &agent_new), (&team, &team_new)] {
        let got = req(
            &mut ws,
            "head",
            json!({"kinds": [30180], "authors": [author], "#d": [d]}),
        )
        .await;
        assert_eq!(ids(&got), vec![head.id.to_hex()], "head for {d}");
    }
    let all = req(
        &mut ws,
        "all",
        json!({"kinds": [30180], "authors": [author]}),
    )
    .await;
    let mut want = vec![agent_new.id.to_hex(), team_new.id.to_hex()];
    let mut got = ids(&all);
    want.sort();
    got.sort();
    assert_eq!(got, want, "one head per address");
    ws.disconnect().await.expect("disconnect");
}

/// Historical REQ, `ids` lookup and WS COUNT: the owner sees the record; a
/// non-author's 30180-only filter is refused, and a mixed-kind or `ids`
/// filter that would match it returns nothing and counts zero.
#[tokio::test]
#[ignore]
async fn test_owner_record_author_only_ws_reads() {
    let url = relay_url();
    let owner = Keys::generate();
    let outsider = Keys::generate();
    let d = agent_d();
    let event = record(&owner, &d, "{}", now());
    let id = event.id.to_hex();

    let mut owner_ws = BuzzTestClient::connect(&url, &owner)
        .await
        .expect("connect");
    publish(&mut owner_ws, event).await;
    let mut outsider_ws = BuzzTestClient::connect(&url, &outsider)
        .await
        .expect("connect");

    let author = owner.public_key().to_hex();
    let by_kind = json!({"kinds": [30180], "authors": [author], "#d": [d]});
    // Mixed with public kind:1 so the filter is not closed up front and the
    // per-event author-only gate is what hides the record.
    let by_tag = json!({"kinds": [1, 30180], "authors": [author], "#d": [d]});
    let by_id = json!({"ids": [id]});

    for filter in [&by_kind, &by_tag, &by_id] {
        assert_eq!(
            ids(&req(&mut owner_ws, "owner", filter.clone()).await),
            vec![id.clone()],
            "owner read {filter}"
        );
        assert_eq!(
            ws_count(&mut owner_ws, filter.clone()).await,
            1,
            "owner count {filter}"
        );
    }
    for filter in [&by_tag, &by_id] {
        assert!(
            req(&mut outsider_ws, "outsider", filter.clone())
                .await
                .is_empty(),
            "non-author must not see the record via {filter}"
        );
        assert_eq!(
            ws_count(&mut outsider_ws, filter.clone()).await,
            0,
            "outsider count {filter}"
        );
    }

    let sid = sub_id("outsider-kind");
    outsider_ws
        .send_raw(&json!(["REQ", sid, by_kind]))
        .await
        .expect("send REQ");
    match outsider_ws
        .recv_event(Duration::from_secs(5))
        .await
        .expect("recv")
    {
        RelayMessage::Closed { message, .. } => {
            assert!(message.starts_with("restricted:"), "{message}")
        }
        other => panic!("expected CLOSED for another author's records, got {other:?}"),
    }

    owner_ws.disconnect().await.expect("disconnect");
    outsider_ws.disconnect().await.expect("disconnect");
}

/// HTTP bridge `/query` and `/count`: the owner sees and counts the record;
/// a non-author's matching mixed-kind and `ids` queries return nothing and
/// count zero.
#[tokio::test]
#[ignore]
async fn test_owner_record_author_only_http_reads() {
    let owner = Keys::generate();
    let outsider = Keys::generate();
    let d = team_d();
    let event = record(&owner, &d, "{}", now());
    let id = event.id.to_hex();
    let mut ws = BuzzTestClient::connect(&relay_url(), &owner)
        .await
        .expect("connect");
    publish(&mut ws, event).await;
    ws.disconnect().await.expect("disconnect");

    let author = owner.public_key().to_hex();
    for filter in [
        json!({"kinds": [1, 30180], "authors": [author], "#d": [d]}),
        json!({"ids": [id]}),
    ] {
        let (status, body) = http_post(&owner, "/query", json!([filter])).await;
        assert_eq!(status, 200);
        assert_eq!(body[0]["id"], json!(id), "owner query {filter}");
        let (_, body) = http_post(&owner, "/count", json!([filter])).await;
        assert_eq!(body["count"], json!(1), "owner count {filter}");

        let (status, body) = http_post(&outsider, "/query", json!([filter])).await;
        assert_eq!(status, 200);
        assert_eq!(body, json!([]), "non-author query {filter}");
        let (_, body) = http_post(&outsider, "/count", json!([filter])).await;
        assert_eq!(body["count"], json!(0), "non-author count {filter}");
    }
}

/// Live fan-out reaches the owner's subscription but not a non-author's
/// mixed-kind subscription on the same author. A public kind:1 note proves
/// the non-author's subscription is live.
#[tokio::test]
#[ignore]
async fn test_owner_record_live_fanout_is_author_only() {
    let url = relay_url();
    let owner = Keys::generate();
    let outsider = Keys::generate();

    let mut owner_ws = BuzzTestClient::connect(&url, &owner)
        .await
        .expect("connect");
    let mut outsider_ws = BuzzTestClient::connect(&url, &outsider)
        .await
        .expect("connect");
    let owner_sid = sub_id("owner-live");
    let outsider_sid = sub_id("outsider-live");
    for (ws, sid) in [
        (&mut owner_ws, &owner_sid),
        (&mut outsider_ws, &outsider_sid),
    ] {
        ws.subscribe(
            sid,
            vec![Filter::new()
                .kinds([Kind::TextNote, Kind::Custom(OWNER_RECORD_KIND)])
                .author(owner.public_key())
                .since(Timestamp::now())],
        )
        .await
        .expect("subscribe");
        ws.collect_until_eose(sid, Duration::from_secs(5))
            .await
            .expect("eose");
    }

    let event = record(&owner, &agent_d(), "{}", now());
    let record_id = event.id;
    publish(&mut owner_ws, event).await;
    let control = EventBuilder::text_note("public control")
        .sign_with_keys(&owner)
        .unwrap();
    let control_id = control.id;
    publish(&mut owner_ws, control).await;

    let mut owner_seen = HashSet::new();
    while owner_seen.len() < 2 {
        if let RelayMessage::Event {
            subscription_id,
            event,
        } = owner_ws
            .recv_event(Duration::from_secs(5))
            .await
            .expect("owner live")
        {
            if subscription_id == owner_sid {
                owner_seen.insert(event.id);
            }
        }
    }
    assert!(
        owner_seen.contains(&record_id),
        "owner must receive the record live"
    );

    loop {
        if let RelayMessage::Event {
            subscription_id,
            event,
        } = outsider_ws
            .recv_event(Duration::from_secs(5))
            .await
            .expect("outsider live")
        {
            if subscription_id != outsider_sid {
                continue;
            }
            assert_ne!(event.id, record_id, "non-author received the record live");
            if event.id == control_id {
                break;
            }
        }
    }

    owner_ws.disconnect().await.expect("disconnect");
    outsider_ws.disconnect().await.expect("disconnect");
}

/// Full-text search never returns a record, not to its author and not to
/// anyone else, over WS REQ and HTTP `/query`.
#[tokio::test]
#[ignore]
async fn test_owner_record_unsearchable_for_everyone() {
    let owner = Keys::generate();
    let token = format!("orsearch{}", &owner.public_key().to_hex()[..16]);
    let event = record(
        &owner,
        &agent_d(),
        &format!(r#"{{"note":"{token}"}}"#),
        now(),
    );
    let mut ws = BuzzTestClient::connect(&relay_url(), &owner)
        .await
        .expect("connect");
    publish(&mut ws, event).await;
    ws.disconnect().await.expect("disconnect");

    let filter = json!({"kinds": [OWNER_RECORD_KIND], "authors": [owner.public_key().to_hex()], "search": token});
    let mixed = json!({"kinds": [1, OWNER_RECORD_KIND], "search": token});
    for (name, reader) in [("author", &owner), ("other", &Keys::generate())] {
        let mut ws = BuzzTestClient::connect(&relay_url(), reader)
            .await
            .expect("connect");
        for f in [&filter, &mixed] {
            if name == "other" && f == &filter {
                // A non-author's 30180-only filter is refused before search.
                continue;
            }
            let hits = req(&mut ws, "search", f.clone()).await;
            assert!(hits.is_empty(), "{name} WS search {f}: {:?}", ids(&hits));
            let (status, body) = http_post(reader, "/query", json!([f])).await;
            assert_eq!(status, 200, "{name} HTTP search {f}: {body}");
            assert_eq!(body, json!([]), "{name} HTTP search {f}");
        }
        ws.disconnect().await.expect("disconnect");
    }
}

/// Every deletion naming a record must carry `["k","30180"]`: by coordinate
/// (as the only target, or alongside an `e` target the relay would act on
/// instead) and by id, while the record is live and after it is deleted.
/// Missing, wrong and noncanonical `k` values are all rejected.
#[tokio::test]
#[ignore]
async fn test_owner_record_deletion_requires_k_tag() {
    let client = aod::http_client();
    let owner = Keys::generate();
    let d = agent_d();
    let event = record(&owner, &d, "{}", now());
    aod::submit_ok(&client, &owner, &event).await;
    let note = EventBuilder::text_note("public note")
        .sign_with_keys(&owner)
        .unwrap();
    aod::submit_ok(&client, &owner, &note).await;
    let by_coord = aod::tag(&["a", &coordinate(&owner, &d)]);
    let by_id = aod::tag(&["e", &event.id.to_hex()]);
    let note_target = aod::tag(&["e", &note.id.to_hex()]);
    let k = OWNER_RECORD_KIND.to_string();

    for tags in [
        vec![by_coord.clone()],
        vec![by_coord.clone(), aod::tag(&["k", "1"])],
        vec![by_coord.clone(), aod::tag(&["k", &format!("0{k}")])],
        vec![by_id.clone()],
        vec![note_target.clone(), by_coord.clone(), aod::tag(&["k", "1"])],
    ] {
        let (accepted, msg) =
            aod::submit(&client, &owner, &aod::build_deletion(&owner, tags.clone())).await;
        assert!(!accepted, "deletion {tags:?} must be rejected (got: {msg})");
    }

    aod::submit_ok(
        &client,
        &owner,
        &aod::build_deletion(&owner, vec![by_coord.clone(), aod::tag(&["k", &k])]),
    )
    .await;
    for tags in [vec![by_coord], vec![by_id]] {
        let (accepted, msg) =
            aod::submit(&client, &owner, &aod::build_deletion(&owner, tags.clone())).await;
        assert!(
            !accepted,
            "untagged deletion {tags:?} of a deleted record must be rejected (got: {msg})"
        );
    }
}

/// A deletion naming a private address as its second `a` tag is refused by
/// the one-target rule before any privacy check runs, for `30180` and for an
/// existing private kind (`30300`).
#[tokio::test]
#[ignore]
async fn test_deletion_with_private_second_coordinate_is_refused() {
    let client = aod::http_client();
    let owner = Keys::generate();
    let pubkey = owner.public_key().to_hex();
    let public = aod::tag(&["a", &format!("30023:{pubkey}:public")]);
    for private in [
        coordinate(&owner, &agent_d()),
        format!("30300:{pubkey}:reminder"),
    ] {
        let tags = vec![public.clone(), aod::tag(&["a", &private])];
        let (accepted, msg) =
            aod::submit(&client, &owner, &aod::build_deletion(&owner, tags)).await;
        assert!(
            !accepted,
            "two-coordinate deletion naming {private} must be refused"
        );
        assert!(
            msg.starts_with("invalid: deletion events must reference exactly one target"),
            "unexpected refusal for {private}: {msg}"
        );
    }
}

/// A coordinate deletion removes the record for its owner and is visible to
/// the owner and nobody else on WS REQ, `ids`, WS COUNT, HTTP `/query` and
/// HTTP `/count`. A `k`-tagged deletion of a coordinate that never held a
/// record is just as private.
#[tokio::test]
#[ignore]
async fn test_owner_record_deletion_reads_are_author_only() {
    let client = aod::http_client();
    let owner = Keys::generate();
    let d = team_d();
    let event = record(&owner, &d, "{}", now());
    aod::submit_ok(&client, &owner, &event).await;
    let k = aod::tag(&["k", &OWNER_RECORD_KIND.to_string()]);
    let deletion = aod::build_deletion(
        &owner,
        vec![aod::tag(&["a", &coordinate(&owner, &d)]), k.clone()],
    );
    aod::submit_ok(&client, &owner, &deletion).await;
    aod::assert_deletion_visibility(&owner, &owner, deletion.id, true).await;
    aod::assert_deletion_visibility(&Keys::generate(), &owner, deletion.id, false).await;

    let by_id = Filter::new().id(event.id);
    assert!(
        aod::http_query_ids(&client, &owner, &by_id)
            .await
            .is_empty(),
        "the deleted record must be gone for its owner"
    );

    let fresh = Keys::generate();
    let never_stored = aod::build_deletion(
        &fresh,
        vec![aod::tag(&["a", &coordinate(&fresh, &agent_d())]), k],
    );
    aod::submit_ok(&client, &fresh, &never_stored).await;
    aod::assert_deletion_visibility(&fresh, &fresh, never_stored.id, true).await;
    aod::assert_deletion_visibility(&Keys::generate(), &fresh, never_stored.id, false).await;
}

/// Subscribed while the record exists, the owner receives its deletion live
/// and another reader does not. A public deletion by the same owner is the
/// positive control: both subscribers must receive it.
#[tokio::test]
#[ignore]
async fn test_owner_record_deletion_live_fanout_is_author_only() {
    let client = aod::http_client();
    let owner = Keys::generate();
    let d = agent_d();
    aod::submit_ok(&client, &owner, &record(&owner, &d, "{}", now())).await;

    let live = Filter::new()
        .kind(Kind::EventDeletion)
        .author(owner.public_key());
    let mut subscribers = Vec::new();
    for keys in [owner.clone(), Keys::generate()] {
        let mut ws = BuzzTestClient::connect(&relay_url(), &keys)
            .await
            .expect("connect");
        let sid = sub_id("deletion-live");
        ws.subscribe(&sid, vec![live.clone()]).await.expect("REQ");
        assert!(ws
            .collect_until_eose(&sid, Duration::from_secs(5))
            .await
            .expect("EOSE")
            .is_empty());
        subscribers.push((ws, sid));
    }

    let deletion = aod::build_deletion(
        &owner,
        vec![
            aod::tag(&["a", &coordinate(&owner, &d)]),
            aod::tag(&["k", &OWNER_RECORD_KIND.to_string()]),
        ],
    );
    aod::submit_ok(&client, &owner, &deletion).await;
    let note = EventBuilder::text_note("public note")
        .sign_with_keys(&owner)
        .unwrap();
    aod::submit_ok(&client, &owner, &note).await;
    let public = aod::build_deletion(
        &owner,
        vec![aod::tag(&["e", &note.id.to_hex()]), aod::tag(&["k", "1"])],
    );
    aod::submit_ok(&client, &owner, &public).await;

    for (i, (mut ws, sid)) in subscribers.into_iter().enumerate() {
        let delivered = aod::live_event_ids(&mut ws, &sid).await;
        assert!(
            delivered.contains(&public.id),
            "public deletion must reach subscriber {i} (0 = owner)"
        );
        assert_eq!(
            delivered.contains(&deletion.id),
            i == 0,
            "private deletion delivery to subscriber {i} (0 = owner)"
        );
        ws.disconnect().await.expect("disconnect");
    }
}

/// A non-author cannot delete a record. By id, the refusal is the one a
/// missing id gets, so it does not reveal that the record exists; by the
/// owner's coordinate it is refused without any lookup. The record survives.
#[tokio::test]
#[ignore]
async fn test_owner_record_non_author_deletion_rejected() {
    let client = aod::http_client();
    let owner = Keys::generate();
    let outsider = Keys::generate();
    let d = agent_d();
    let event = record(&owner, &d, "{}", now());
    aod::submit_ok(&client, &owner, &event).await;
    let k = aod::tag(&["k", &OWNER_RECORD_KIND.to_string()]);

    let missing = EventBuilder::text_note("never stored")
        .sign_with_keys(&owner)
        .unwrap()
        .id;
    let (_, missing_msg) = aod::submit(
        &client,
        &outsider,
        &aod::build_deletion(
            &outsider,
            vec![aod::tag(&["e", &missing.to_hex()]), k.clone()],
        ),
    )
    .await;
    let (accepted, by_id_msg) = aod::submit(
        &client,
        &outsider,
        &aod::build_deletion(
            &outsider,
            vec![aod::tag(&["e", &event.id.to_hex()]), k.clone()],
        ),
    )
    .await;
    assert!(!accepted, "non-author deletion by id must be rejected");
    assert_eq!(by_id_msg, missing_msg, "refusal must match a missing id's");

    let (accepted, msg) = aod::submit(
        &client,
        &outsider,
        &aod::build_deletion(
            &outsider,
            vec![aod::tag(&["a", &coordinate(&owner, &d)]), k],
        ),
    )
    .await;
    assert!(
        !accepted,
        "non-author deletion by coordinate must be rejected (got: {msg})"
    );

    let still = aod::http_query_ids(&client, &owner, &Filter::new().id(event.id)).await;
    assert_eq!(still, vec![event.id.to_hex()], "the record must survive");
}
