//! Shared helpers for NIP-09 deletion e2e tests of author-only kinds. Each
//! test binary covering a kind in `AUTHOR_ONLY_KINDS` includes this module
//! and defines `DEFAULT_RELAY_URL`, so the helpers and the including suite
//! always talk to the same relay.
#![allow(dead_code)]

use std::time::Duration;

use buzz_test_client::{BuzzTestClient, RelayMessage, TestClientError};
use nostr::{EventBuilder, EventId, Filter, Keys, Kind, Tag};
use reqwest::Client;
use serde_json::Value;

pub fn relay_url() -> String {
    std::env::var("RELAY_URL").unwrap_or_else(|_| super::DEFAULT_RELAY_URL.to_string())
}

pub fn relay_http_url() -> String {
    relay_url()
        .replace("wss://", "https://")
        .replace("ws://", "http://")
        .trim_end_matches('/')
        .to_string()
}

pub fn sub_id(name: &str) -> String {
    format!("e2e-aod-{name}-{}", uuid::Uuid::new_v4())
}

pub fn http_client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("failed to build HTTP client")
}

pub fn tag(parts: &[&str]) -> Tag {
    Tag::parse(parts.to_vec()).expect("valid tag")
}

pub fn build_deletion(keys: &Keys, tags: Vec<Tag>) -> nostr::Event {
    build_deletion_with_reason(keys, tags, "")
}

pub fn build_deletion_with_reason(keys: &Keys, tags: Vec<Tag>, reason: &str) -> nostr::Event {
    EventBuilder::new(Kind::EventDeletion, reason)
        .tags(tags)
        .sign_with_keys(keys)
        .unwrap()
}

pub async fn submit(client: &Client, keys: &Keys, event: &nostr::Event) -> (bool, String) {
    let resp = client
        .post(format!("{}/events", relay_http_url()))
        .header("X-Pubkey", keys.public_key().to_hex())
        .json(event)
        .send()
        .await
        .expect("submit event");
    let ok = resp.status().is_success();
    let body: Value = resp.json().await.expect("parse response");
    if ok {
        (
            body["accepted"].as_bool().unwrap_or(false),
            body["message"].as_str().unwrap_or("").to_string(),
        )
    } else {
        (false, body["error"].as_str().unwrap_or("").to_string())
    }
}

pub async fn submit_ok(client: &Client, keys: &Keys, event: &nostr::Event) {
    let (accepted, msg) = submit(client, keys, event).await;
    assert!(accepted, "setup event rejected: {msg}");
}

/// Filters that each match only the deletion `store_deleted_reminder` left:
/// by kind, mixed kinds, and id. The id filter is the kindless COUNT case;
/// broader kindless filters can match `#p`-gated kinds, so the relay closes
/// them before any read runs.
pub fn deletion_filters(author: &Keys, deletion: EventId) -> Vec<Filter> {
    vec![
        Filter::new()
            .kind(Kind::EventDeletion)
            .author(author.public_key()),
        Filter::new()
            .kinds([Kind::EventDeletion, Kind::TextNote])
            .author(author.public_key()),
        Filter::new().id(deletion),
    ]
}

pub async fn http_query_ids(client: &Client, reader: &Keys, filter: &Filter) -> Vec<String> {
    let resp = client
        .post(format!("{}/query", relay_http_url()))
        .header("X-Pubkey", reader.public_key().to_hex())
        .json(&vec![filter])
        .send()
        .await
        .expect("query events");
    assert!(
        resp.status().is_success(),
        "query failed: {}",
        resp.status()
    );
    resp.json::<Vec<Value>>()
        .await
        .expect("parse query response")
        .iter()
        .filter_map(|e| e["id"].as_str().map(str::to_string))
        .collect()
}

pub async fn http_count(client: &Client, reader: &Keys, filter: &Filter) -> u64 {
    let resp = client
        .post(format!("{}/count", relay_http_url()))
        .header("X-Pubkey", reader.public_key().to_hex())
        .json(&vec![filter])
        .send()
        .await
        .expect("count events");
    assert!(
        resp.status().is_success(),
        "count failed: {}",
        resp.status()
    );
    let body: Value = resp.json().await.expect("parse count response");
    body["count"].as_u64().expect("count field")
}

pub async fn ws_query_ids(ws: &mut BuzzTestClient, filter: &Filter) -> Vec<EventId> {
    let sid = sub_id("req");
    ws.subscribe(&sid, vec![filter.clone()]).await.expect("REQ");
    let events = ws
        .collect_until_eose(&sid, Duration::from_secs(5))
        .await
        .expect("EOSE");
    ws.close_subscription(&sid).await.expect("CLOSE");
    events.into_iter().map(|e| e.id).collect()
}

pub async fn ws_count(ws: &mut BuzzTestClient, filter: &Filter) -> u64 {
    let sid = sub_id("count");
    ws.send_raw(&serde_json::json!(["COUNT", sid, filter]))
        .await
        .expect("COUNT");
    loop {
        match ws
            .recv_event(Duration::from_secs(5))
            .await
            .expect("COUNT reply")
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

/// Every historical read surface returns the deletion to `reader` iff
/// `visible`: WS REQ, WS COUNT, HTTP `/query` and HTTP `/count`, by author
/// and by id.
pub async fn assert_deletion_visibility(
    reader: &Keys,
    author: &Keys,
    deletion: EventId,
    visible: bool,
) {
    let client = http_client();
    let mut ws = BuzzTestClient::connect(&relay_url(), reader)
        .await
        .expect("connect reader");
    let expected = u64::from(visible);
    let mut mismatches = Vec::new();
    for filter in deletion_filters(author, deletion) {
        let observed = [
            ("WS REQ", ws_query_ids(&mut ws, &filter).await.len() as u64),
            ("WS COUNT", ws_count(&mut ws, &filter).await),
            (
                "HTTP /query",
                http_query_ids(&client, reader, &filter).await.len() as u64,
            ),
            ("HTTP /count", http_count(&client, reader, &filter).await),
        ];
        for (surface, n) in observed {
            if n != expected {
                mismatches.push(format!("{surface} {filter:?}: {n}, expected {expected}"));
            }
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    ws.disconnect().await.expect("disconnect");
}

/// IDs of the events subscription `sid` receives until 2s pass with no
/// message. A dropped connection or a `CLOSED` for `sid` fails the test, so
/// a dead subscription can never pass for one that saw nothing.
pub async fn live_event_ids(ws: &mut BuzzTestClient, sid: &str) -> Vec<EventId> {
    let mut ids = Vec::new();
    loop {
        match ws.recv_event(Duration::from_secs(2)).await {
            Ok(RelayMessage::Event {
                subscription_id,
                event,
            }) if subscription_id == sid => ids.push(event.id),
            Ok(RelayMessage::Closed {
                subscription_id,
                message,
            }) if subscription_id == sid => panic!("{sid} closed: {message}"),
            Ok(_) => {}
            Err(TestClientError::Timeout) => return ids,
            Err(e) => panic!("{sid} receive failed: {e}"),
        }
    }
}
