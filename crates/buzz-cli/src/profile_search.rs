//! Finding people by profile name.
//!
//! NIP-50 search over kind:0 matches the whole profile JSON, so text inside
//! avatar data URLs, `about` or `nip05` competes with real names: a shared
//! default avatar can fill every result page and hide the person a name
//! lookup asked for. The relay's `POST /profiles/search` matches only `name`
//! and `display_name` and puts exact and prefix name matches first.

use crate::client::BuzzClient;
use crate::error::CliError;

/// The relay's name search path.
const SEARCH_PATH: &str = "/profiles/search";
/// Longest query the name search accepts, in characters after trimming.
const QUERY_MAX_CHARS: usize = 100;
/// Largest page the name search returns.
const LIMIT_MAX: u32 = 100;

/// Returns up to `limit` kind:0 events whose names match `query`, as the raw
/// JSON array the relay sent.
///
/// Uses the relay's name search. A relay without it answers 404 or 405; the
/// search then uses the NIP-50 kind:0 search that callers used before.
/// Requests the name search cannot take (a query over 100 characters, or a
/// `limit` over 100) also use NIP-50, as before. Other errors are returned.
/// Callers still filter the events by name.
pub(crate) async fn search_profiles(
    client: &BuzzClient,
    query: &str,
    limit: u32,
) -> Result<String, CliError> {
    let text = query.trim();
    if !text.is_empty()
        && text.chars().count() <= QUERY_MAX_CHARS
        && (1..=LIMIT_MAX).contains(&limit)
    {
        let body = serde_json::json!({ "query": text, "limit": limit });
        match client.post_json_authed(SEARCH_PATH, &body).await {
            Ok(raw) => return Ok(raw),
            Err(CliError::Relay {
                status: 404 | 405, ..
            }) => {}
            Err(error) => return Err(error),
        }
    }
    client
        .query(&serde_json::json!({
            "kinds": [0],
            "search": query,
            "limit": limit,
        }))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Bytes;
    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::routing::post;
    use axum::Router;
    use nostr::Keys;
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;

    type Seen = Arc<Mutex<Vec<(String, serde_json::Value)>>>;

    /// A relay whose name search answers `status`; `/query` answers `[]`.
    async fn relay(status: StatusCode) -> (BuzzClient, Seen) {
        let seen: Seen = Arc::default();
        let app = Router::new()
            .route(
                SEARCH_PATH,
                post(move |State(seen): State<Seen>, body: Bytes| async move {
                    let body = serde_json::from_slice(&body).unwrap();
                    seen.lock().unwrap().push((SEARCH_PATH.into(), body));
                    (status, r#"[{"kind":0,"from":"names"}]"#)
                }),
            )
            .route(
                "/query",
                post(|State(seen): State<Seen>, body: Bytes| async move {
                    let body = serde_json::from_slice(&body).unwrap();
                    seen.lock().unwrap().push(("/query".into(), body));
                    "[]"
                }),
            )
            .with_state(seen.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async { axum::serve(listener, app).await.unwrap() });
        let client = BuzzClient::new(url, Keys::generate(), None, None).unwrap();
        (client, seen)
    }

    fn nip50(query: &str, limit: u32) -> (String, serde_json::Value) {
        (
            "/query".into(),
            serde_json::json!([{ "kinds": [0], "search": query, "limit": limit }]),
        )
    }

    #[tokio::test]
    async fn uses_the_name_search() {
        let (client, seen) = relay(StatusCode::OK).await;
        let raw = search_profiles(&client, " Wes ", 100).await.unwrap();
        assert_eq!(raw, r#"[{"kind":0,"from":"names"}]"#);
        assert_eq!(
            *seen.lock().unwrap(),
            [(
                SEARCH_PATH.to_string(),
                serde_json::json!({ "query": "Wes", "limit": 100 })
            )]
        );
    }

    #[tokio::test]
    async fn a_relay_without_the_name_search_uses_nip50() {
        for status in [StatusCode::NOT_FOUND, StatusCode::METHOD_NOT_ALLOWED] {
            let (client, seen) = relay(status).await;
            assert_eq!(search_profiles(&client, "Wes", 100).await.unwrap(), "[]");
            let seen = seen.lock().unwrap();
            assert_eq!(seen.len(), 2);
            assert_eq!(seen[1], nip50("Wes", 100));
        }
    }

    #[tokio::test]
    async fn other_name_search_errors_are_returned() {
        let (client, seen) = relay(StatusCode::INTERNAL_SERVER_ERROR).await;
        let error = search_profiles(&client, "Wes", 100).await.unwrap_err();
        assert!(matches!(error, CliError::Relay { status: 500, .. }));
        assert!(seen
            .lock()
            .unwrap()
            .iter()
            .all(|(path, _)| path != "/query"));
    }

    #[tokio::test]
    async fn requests_the_name_search_cannot_take_use_nip50() {
        let long = "w".repeat(QUERY_MAX_CHARS + 1);
        for (query, limit) in [(long.as_str(), 100), ("Wes", 101), ("  ", 100)] {
            let (client, seen) = relay(StatusCode::OK).await;
            search_profiles(&client, query, limit).await.unwrap();
            assert_eq!(*seen.lock().unwrap(), [nip50(query, limit)]);
        }
    }
}
