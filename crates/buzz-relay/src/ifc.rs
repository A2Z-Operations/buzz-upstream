//! Strict remote-agent information-flow query proofs.

use std::collections::HashSet;

use axum::http::StatusCode;
use buzz_core::kind::{
    KIND_CANVAS, KIND_IFC_ATTESTATION, KIND_STREAM_MESSAGE, KIND_STREAM_MESSAGE_V2,
};
use buzz_core::{CommunityId, TenantContext};
use buzz_db::ifc::IfcError;
use buzz_db::EventQuery;
use chrono::{DateTime, Utc};
use nostr::{Event, EventBuilder, Kind, PublicKey, Tag};
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::state::AppState;

const HISTORY_KINDS: [u32; 3] = [KIND_STREAM_MESSAGE, KIND_STREAM_MESSAGE_V2, KIND_CANVAS];
/// Maximum serialized attestation content returned by one query.
pub(crate) const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
/// End-to-end deadline for one proof query.
pub(crate) const QUERY_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Debug)]
struct Request {
    channel_id: Uuid,
    nonce: String,
    expected_generation: Option<i64>,
    policy_only: bool,
    query: EventQuery,
}

#[derive(Serialize)]
struct Content<'a> {
    version: u8,
    community_id: String,
    requester: &'a str,
    nonce: &'a str,
    request_sha256: String,
    policy: Policy<'a>,
    resources: Vec<Resource<'a>>,
}

#[derive(Serialize)]
struct Policy<'a> {
    channel_id: String,
    generation: String,
    channel_type: &'a str,
    visibility: &'a str,
    history_access: &'a str,
    archived: bool,
    members: &'a [String],
}

#[derive(Serialize)]
struct Resource<'a> {
    event: &'a Event,
    effective_generation: String,
    origin_generation: Option<String>,
}

fn bad(message: impl Into<String>) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, message.into())
}

fn positive_decimal(value: &Value, field: &str) -> Result<i64, (StatusCode, String)> {
    let text = value
        .as_str()
        .ok_or_else(|| bad(format!("{field} must be a decimal string")))?;
    if text.is_empty() || text.starts_with('0') || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(bad(format!("{field} is not canonical")));
    }
    text.parse::<i64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| bad(format!("{field} is outside the supported range")))
}

fn string_array(value: &Value, field: &str) -> Result<Vec<String>, (StatusCode, String)> {
    value
        .as_array()
        .ok_or_else(|| bad(format!("{field} must be an array")))?
        .iter()
        .map(|item| {
            item.as_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| bad(format!("{field} entries must be strings")))
        })
        .collect()
}

fn decode_hex32(value: &str, field: &str) -> Result<Vec<u8>, (StatusCode, String)> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(bad(format!("{field} entries must be 32-byte hex")));
    }
    hex::decode(value).map_err(|_| bad(format!("{field} entries must be hex")))
}

fn parse_request(body: &[u8], community_id: CommunityId) -> Result<Request, (StatusCode, String)> {
    let value = strict_json(body).map_err(|error| bad(format!("invalid IFC query: {error}")))?;
    let filters = value
        .as_array()
        .filter(|filters| filters.len() == 1)
        .ok_or_else(|| bad("IFC query requires exactly one filter"))?;
    let object = filters[0]
        .as_object()
        .ok_or_else(|| bad("IFC filter must be an object"))?;
    let allowed = [
        "kinds", "#h", "limit", "since", "until", "ids", "authors", "#e", "ifc",
    ];
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(bad("IFC filter contains an unsupported field"));
    }
    let extension = object
        .get("ifc")
        .and_then(Value::as_object)
        .ok_or_else(|| bad("IFC extension must be an object"))?;
    if extension
        .keys()
        .any(|key| !["version", "nonce", "expected_generation"].contains(&key.as_str()))
    {
        return Err(bad("IFC extension contains an unsupported field"));
    }
    if extension.get("version").and_then(Value::as_u64) != Some(1) {
        return Err(bad("unsupported IFC version"));
    }
    let nonce = extension
        .get("nonce")
        .and_then(Value::as_str)
        .filter(|nonce| {
            nonce.len() == 32
                && nonce
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        })
        .ok_or_else(|| bad("IFC nonce must be 32 lowercase hex characters"))?
        .to_owned();
    let expected_generation = extension
        .get("expected_generation")
        .map(|value| positive_decimal(value, "expected_generation"))
        .transpose()?;
    let channels = string_array(object.get("#h").ok_or_else(|| bad("missing #h"))?, "#h")?;
    if channels.len() != 1 {
        return Err(bad("IFC query requires exactly one #h channel"));
    }
    let channel_id = Uuid::parse_str(&channels[0]).map_err(|_| bad("#h is not a UUID"))?;
    if channel_id.to_string() != channels[0] {
        return Err(bad("#h UUID is not canonical"));
    }
    let kinds = object
        .get("kinds")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("IFC query requires kinds"))?
        .iter()
        .map(|kind| {
            kind.as_u64()
                .and_then(|kind| u32::try_from(kind).ok())
                .ok_or_else(|| bad("kind is invalid"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let limit = object
        .get("limit")
        .and_then(Value::as_i64)
        .ok_or_else(|| bad("IFC query requires a limit"))?;
    let policy_only = kinds == [KIND_IFC_ATTESTATION] && limit == 1;
    if !policy_only
        && (kinds.is_empty()
            || kinds.iter().any(|kind| !HISTORY_KINDS.contains(kind))
            || limit <= 0
            || limit > buzz_db::ifc::IFC_MAX_RESOURCES)
    {
        return Err(bad("unsupported IFC history query"));
    }
    if policy_only
        && object
            .keys()
            .any(|key| !["kinds", "#h", "limit", "ifc"].contains(&key.as_str()))
    {
        return Err(bad("policy-only IFC query contains history filters"));
    }

    let mut query = EventQuery::for_community(community_id);
    query.channel_id = Some(channel_id);
    query.kinds = Some(if policy_only {
        Vec::new()
    } else {
        kinds.iter().map(|kind| *kind as i32).collect()
    });
    query.limit = Some(if policy_only { 0 } else { limit });
    query.max_limit = Some(buzz_db::ifc::IFC_MAX_RESOURCES);
    query.since = object
        .get("since")
        .map(|value| parse_timestamp(value, "since"))
        .transpose()?;
    query.until = object
        .get("until")
        .map(|value| parse_timestamp(value, "until"))
        .transpose()?;
    query.ids = object
        .get("ids")
        .map(|value| {
            string_array(value, "ids")?
                .iter()
                .map(|value| decode_hex32(value, "ids"))
                .collect()
        })
        .transpose()?;
    query.authors = object
        .get("authors")
        .map(|value| {
            string_array(value, "authors")?
                .iter()
                .map(|value| decode_hex32(value, "authors"))
                .collect()
        })
        .transpose()?;
    query.e_tags = object
        .get("#e")
        .map(|value| string_array(value, "#e"))
        .transpose()?;
    Ok(Request {
        channel_id,
        nonce,
        expected_generation,
        policy_only,
        query,
    })
}

fn parse_timestamp(value: &Value, field: &str) -> Result<DateTime<Utc>, (StatusCode, String)> {
    let seconds = value
        .as_i64()
        .ok_or_else(|| bad(format!("{field} must be an integer")))?;
    DateTime::from_timestamp(seconds, 0)
        .ok_or_else(|| bad(format!("{field} is outside the supported range")))
}

/// Return `None` only when the request does not opt into IFC.
pub(crate) async fn query(
    state: &AppState,
    tenant: &TenantContext,
    requester: &PublicKey,
    body: &[u8],
) -> Option<Result<Value, (StatusCode, String)>> {
    let opts_in = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| value.as_array().cloned())
        .is_some_and(|filters| {
            filters.iter().any(|filter| {
                filter
                    .as_object()
                    .is_some_and(|object| object.contains_key("ifc"))
            })
        });
    if !opts_in {
        return None;
    }
    Some(
        tokio::time::timeout(QUERY_DEADLINE, query_inner(state, tenant, requester, body))
            .await
            .unwrap_or_else(|_| {
                Err((
                    StatusCode::SERVICE_UNAVAILABLE,
                    "ifc-unavailable".to_owned(),
                ))
            }),
    )
}

async fn query_inner(
    state: &AppState,
    tenant: &TenantContext,
    requester: &PublicKey,
    body: &[u8],
) -> Result<Value, (StatusCode, String)> {
    let request = parse_request(body, tenant.community())?;
    let requester_hex = requester.to_hex();
    let cut = state
        .db
        .ifc_read_cut(
            tenant.community(),
            request.channel_id,
            &requester_hex,
            request.expected_generation,
            &request.query,
        )
        .await
        .map_err(map_db_error)?;
    if request.policy_only && !cut.resources.is_empty() {
        return Err((StatusCode::SERVICE_UNAVAILABLE, "ifc-invalid".to_owned()));
    }
    for resource in &cut.resources {
        if resource.channel_id != Some(request.channel_id)
            || !HISTORY_KINDS.contains(&(resource.event.kind.as_u16() as u32))
            || !resource.event.verify_id()
            || !resource.event.verify_signature()
        {
            return Err((StatusCode::SERVICE_UNAVAILABLE, "ifc-invalid".to_owned()));
        }
    }
    let generation = cut.policy.generation.to_string();
    let content = Content {
        version: 1,
        community_id: tenant.community().to_string(),
        requester: &requester_hex,
        nonce: &request.nonce,
        request_sha256: hex::encode(Sha256::digest(body)),
        policy: Policy {
            channel_id: cut.policy.channel_id.to_string(),
            generation: generation.clone(),
            channel_type: &cut.policy.channel_type,
            visibility: &cut.policy.visibility,
            history_access: &cut.policy.history_access,
            archived: cut.policy.archived,
            members: &cut.policy.members,
        },
        resources: cut
            .resources
            .iter()
            .map(|resource| Resource {
                event: &resource.event,
                effective_generation: generation.clone(),
                origin_generation: None,
            })
            .collect(),
    };
    let content = serde_json::to_string(&content)
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "ifc-invalid".to_owned()))?;
    if content.len() > MAX_RESPONSE_BYTES {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            "ifc-response-too-large".to_owned(),
        ));
    }
    let channel_text = request.channel_id.to_string();
    let tags = [
        Tag::parse(["p", requester_hex.as_str()])
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "ifc-invalid".to_owned()))?,
        Tag::parse(["h", channel_text.as_str()])
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "ifc-invalid".to_owned()))?,
    ];
    let attestation = EventBuilder::new(Kind::Custom(KIND_IFC_ATTESTATION as u16), content)
        .tags(tags)
        .sign_with_keys(&state.relay_keypair)
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "ifc-invalid".to_owned()))?;
    Ok(serde_json::json!({"ifc": 1, "attestation": attestation}))
}

fn map_db_error(error: IfcError) -> (StatusCode, String) {
    match error {
        IfcError::PolicyChanged => (StatusCode::CONFLICT, "ifc-policy-changed".to_owned()),
        IfcError::AccessDenied => (
            StatusCode::FORBIDDEN,
            "restricted: channel access denied".to_owned(),
        ),
        IfcError::Invalid => (StatusCode::SERVICE_UNAVAILABLE, "ifc-invalid".to_owned()),
        IfcError::Expired => (StatusCode::BAD_REQUEST, "ifc-expired".to_owned()),
        IfcError::Database(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "ifc-unavailable".to_owned(),
        ),
    }
}

fn strict_json(input: &[u8]) -> Result<Value, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_slice(input);
    let value = NoDuplicates.deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(value)
}

struct NoDuplicates;

impl<'de> DeserializeSeed<'de> for NoDuplicates {
    type Value = Value;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(NoDuplicateVisitor)
    }
}

struct NoDuplicateVisitor;

impl<'de> Visitor<'de> for NoDuplicateVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("JSON without duplicate object fields")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }
    fn visit_i64<E>(self, value: i64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }
    fn visit_u64<E>(self, value: u64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }
    fn visit_f64<E>(self, value: f64) -> Result<Value, E>
    where
        E: serde::de::Error,
    {
        serde_json::Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("non-finite number"))
    }
    fn visit_str<E>(self, value: &str) -> Result<Value, E> {
        Ok(Value::String(value.to_owned()))
    }
    fn visit_string<E>(self, value: String) -> Result<Value, E> {
        Ok(Value::String(value))
    }
    fn visit_none<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_some<D>(self, deserializer: D) -> Result<Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        NoDuplicates.deserialize(deserializer)
    }
    fn visit_seq<A>(self, mut sequence: A) -> Result<Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element_seed(NoDuplicates)? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }
    fn visit_map<A>(self, mut map: A) -> Result<Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = Map::new();
        let mut seen = HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !seen.insert(key.clone()) {
                return Err(serde::de::Error::custom(format!("duplicate field {key}")));
            }
            values.insert(key, map.next_value_seed(NoDuplicates)?);
        }
        Ok(Value::Object(values))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use buzz_core::engram::{conversation_key, d_tag, Body};
    use buzz_core::ifc::{parse_publication_fence, IfcPublicationTarget};
    use nostr::nips::nip44::v2;
    use nostr::Keys;

    #[test]
    fn duplicate_ifc_fields_are_rejected() {
        let body = br##"[{"kinds":[45900],"#h":["11111111-1111-4111-8111-111111111111"],"limit":1,"ifc":{"version":1,"version":1,"nonce":"0123456789abcdef0123456789abcdef"}}]"##;
        assert!(strict_json(body).is_err());
    }

    #[test]
    fn shared_history_request_is_strictly_parsed() {
        let body = br##"[{"kinds":[9,40002,40100],"#h":["11111111-1111-4111-8111-111111111111"],"limit":13,"ifc":{"version":1,"nonce":"0123456789abcdef0123456789abcdef","expected_generation":"17"}}]"##;
        let request = parse_request(body, CommunityId::from_uuid(Uuid::nil())).expect("request");
        assert_eq!(request.expected_generation, Some(17));
        assert!(!request.policy_only);
        assert_eq!(request.query.limit, Some(13));
        assert_eq!(request.query.kinds, Some(vec![9, 40002, 40100]));
    }

    #[test]
    fn unknown_ifc_fields_and_noncanonical_generation_are_rejected() {
        for body in [
            br##"[{"kinds":[45900],"#h":["11111111-1111-4111-8111-111111111111"],"limit":1,"ifc":{"version":1,"nonce":"0123456789abcdef0123456789abcdef","extra":true}}]"##.as_slice(),
            br##"[{"kinds":[45900],"#h":["11111111-1111-4111-8111-111111111111"],"limit":1,"ifc":{"version":1,"nonce":"0123456789abcdef0123456789abcdef","expected_generation":"017"}}]"##.as_slice(),
        ] {
            assert!(parse_request(body, CommunityId::from_uuid(Uuid::nil())).is_err());
        }
    }

    #[test]
    fn http_and_websocket_publish_through_the_shared_ifc_gate() {
        let ingest = include_str!("handlers/ingest.rs");
        let http = include_str!("api/bridge.rs");
        let websocket = include_str!("handlers/event.rs");
        assert!(ingest.contains("parse_publication_fence(&event)"));
        assert!(ingest.contains(".publish_ifc_event("));
        assert!(http.contains("crate::handlers::ingest::ingest_event("));
        assert!(websocket.contains("super::ingest::ingest_event("));
        assert!(websocket.contains(".accept_ifc_ephemeral("));
    }

    #[test]
    fn golden_fixture_has_valid_signatures_and_exact_request_digest() {
        let fixture_text = include_str!("../tests/fixtures/ifc-v1-golden.json");
        assert!(!fixture_text.contains("REPLACE_WITH"));
        let fixture: Value = serde_json::from_str(fixture_text).expect("parse fixture");
        let request = fixture["request_body_utf8"]
            .as_str()
            .expect("request bytes");
        assert_eq!(
            fixture["request_sha256"].as_str(),
            Some(hex::encode(Sha256::digest(request.as_bytes())).as_str())
        );
        let attestation: Event = serde_json::from_value(fixture["response"]["attestation"].clone())
            .expect("attestation event");
        assert!(attestation.verify_id());
        assert!(attestation.verify_signature());
        let content: Value = serde_json::from_str(&attestation.content).expect("content");
        assert_eq!(content["request_sha256"], fixture["request_sha256"]);
        assert!(content["policy"]["generation"].is_string());
        assert!(content["resources"][0]["origin_generation"].is_null());
        let resource: Event = serde_json::from_value(content["resources"][0]["event"].clone())
            .expect("resource event");
        assert!(resource.verify_id());
        assert!(resource.verify_signature());
        assert_eq!(
            content["requester"],
            fixture["test_keys"]["query_requester_pubkey"]
        );
        assert_eq!(
            attestation
                .tags
                .iter()
                .next()
                .expect("requester tag")
                .as_slice()[1],
            content["requester"].as_str().expect("requester")
        );
        assert_ne!(content["requester"], resource.pubkey.to_hex());
        let publication: Event =
            serde_json::from_value(fixture["channel_publication_event"].clone())
                .expect("publication event");
        assert!(publication.verify_id());
        assert!(publication.verify_signature());
        assert!(parse_publication_fence(&publication)
            .expect("parse publication")
            .is_some());

        let memory: Event = serde_json::from_value(fixture["owner_memory"]["event"].clone())
            .expect("owner-memory event");
        assert!(memory.verify_id());
        assert!(memory.verify_signature());
        let memory_fence = parse_publication_fence(&memory)
            .expect("parse owner-memory publication")
            .expect("owner-memory fence");
        let expected_d = fixture["owner_memory"]["d_tag"]
            .as_str()
            .expect("memory d tag");
        assert!(matches!(
            memory_fence.target,
            IfcPublicationTarget::OwnerMemory { ref d_tag, .. } if d_tag == expected_d
        ));
        let broker = Keys::parse(
            fixture["test_keys"]["broker_secret_hex"]
                .as_str()
                .expect("broker secret"),
        )
        .expect("broker keys");
        let human = Keys::parse(
            fixture["test_keys"]["human_secret_hex"]
                .as_str()
                .expect("human secret"),
        )
        .expect("human keys");
        let memory_key = conversation_key(broker.secret_key(), &human.public_key());
        assert_eq!(
            d_tag(
                &memory_key,
                fixture["owner_memory"]["canonical_slug"]
                    .as_str()
                    .expect("memory slug")
            ),
            expected_d
        );
        let payload = base64::engine::general_purpose::STANDARD
            .decode(memory.content.as_bytes())
            .expect("memory base64");
        let plaintext = v2::decrypt_to_bytes(&memory_key, &payload).expect("memory decrypt");
        assert_eq!(
            plaintext,
            fixture["owner_memory"]["plaintext_utf8"]
                .as_str()
                .expect("memory plaintext")
                .as_bytes()
        );
        assert_eq!(
            Body::from_json_bytes(&plaintext)
                .expect("compatible memory body")
                .slug(),
            fixture["owner_memory"]["canonical_slug"]
                .as_str()
                .expect("memory slug")
        );

        let telemetry: Event = serde_json::from_value(fixture["owner_telemetry"]["event"].clone())
            .expect("owner-telemetry event");
        assert!(telemetry.verify_id());
        assert!(telemetry.verify_signature());
        assert_eq!(telemetry.pubkey, broker.public_key());
        assert_ne!(telemetry.pubkey, human.public_key());
        let telemetry_fence = parse_publication_fence(&telemetry)
            .expect("parse owner-telemetry publication")
            .expect("owner-telemetry fence");
        assert!(matches!(
            telemetry_fence.target,
            IfcPublicationTarget::OwnerTelemetry { ref owner_pubkey }
                if owner_pubkey == &human.public_key().to_hex()
        ));
        let telemetry_payload = base64::engine::general_purpose::STANDARD
            .decode(telemetry.content.as_bytes())
            .expect("telemetry base64");
        let telemetry_plaintext =
            v2::decrypt_to_bytes(&memory_key, &telemetry_payload).expect("telemetry decrypt");
        assert_eq!(
            telemetry_plaintext,
            fixture["owner_telemetry"]["plaintext_utf8"]
                .as_str()
                .expect("telemetry plaintext")
                .as_bytes()
        );
    }
}
