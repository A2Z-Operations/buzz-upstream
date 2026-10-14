//! Shared version-one remote-agent information-flow fence grammar.

use std::collections::HashSet;

use nostr::Event;
use thiserror::Error;
use uuid::Uuid;

use crate::kind::{
    event_kind_u32, KIND_AGENT_ENGRAM, KIND_AGENT_OBSERVER_FRAME, KIND_CANVAS, KIND_STREAM_MESSAGE,
    KIND_STREAM_MESSAGE_V2,
};
use crate::observer::{
    content_looks_like_nip44, OBSERVER_AGENT_TAG, OBSERVER_FRAME_TAG, OBSERVER_FRAME_TELEMETRY,
};

/// Version carried by all supported information-flow fence tags.
pub const IFC_VERSION: &str = "1";
/// Maximum distinct source policies on one publication.
pub const IFC_MAX_SOURCES: usize = 8;
/// Maximum future lifetime of a new publication fence.
pub const IFC_MAX_TTL_SECONDS: i64 = 300;
/// Duration for which an accepted event may be retried as a historic duplicate.
pub const IFC_RECEIPT_RETENTION_SECONDS: i64 = 86_400;
/// Channel event kinds that may carry a version-one publication fence.
pub const IFC_CHANNEL_PUBLICATION_KINDS: [u32; 3] =
    [KIND_STREAM_MESSAGE, KIND_STREAM_MESSAGE_V2, KIND_CANVAS];
/// Nonchannel event kind admitted by the closed owner-memory grammar.
pub const IFC_OWNER_MEMORY_KIND: u32 = KIND_AGENT_ENGRAM;
/// Ephemeral event kind admitted by the closed owner-telemetry grammar.
pub const IFC_OWNER_TELEMETRY_KIND: u32 = KIND_AGENT_OBSERVER_FRAME;
/// Every event kind admitted by version-one fenced publication.
pub const IFC_PUBLICATION_KINDS: [u32; 5] = [
    KIND_STREAM_MESSAGE,
    KIND_AGENT_OBSERVER_FRAME,
    KIND_AGENT_ENGRAM,
    KIND_STREAM_MESSAGE_V2,
    KIND_CANVAS,
];

/// One channel policy coordinate asserted by the trusted broker.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct IfcPolicyRef {
    /// Channel whose canonical policy is referenced.
    pub channel_id: Uuid,
    /// Positive monotone generation encoded canonically on the wire.
    pub generation: i64,
}

/// Signed optimistic-concurrency precondition for a fenced owner-memory slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IfcMemoryExpectedHead {
    /// The coordinate must not have a live head.
    Absent,
    /// The coordinate's live head must be this exact event ID.
    Event([u8; 32]),
}

/// Closed destination admitted by a version-one publication fence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IfcPublicationTarget {
    /// Ordinary channel publication.
    Channel(IfcPolicyRef),
    /// Existing owner-encrypted agent-memory coordinate.
    OwnerMemory {
        /// Saved owner asserted by the trusted broker.
        owner_pubkey: String,
        /// Opaque HMAC address used by the existing memory protocol.
        d_tag: String,
        /// Opaque audience namespace carried by the signed event.
        namespace: String,
        /// Exact head or absence frozen into the signed publication.
        expected_head: IfcMemoryExpectedHead,
    },
    /// Existing owner-encrypted ephemeral telemetry route.
    OwnerTelemetry {
        /// Saved owner asserted by the trusted broker.
        owner_pubkey: String,
    },
}

/// Strictly parsed version-one publication fence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IfcPublicationFence {
    /// Host-bound community asserted in the signed event.
    pub community_id: Uuid,
    /// Fence expiry as Unix seconds.
    pub expires_at: i64,
    /// Sorted, distinct source policy coordinates.
    pub sources: Vec<IfcPolicyRef>,
    /// Closed publication destination.
    pub target: IfcPublicationTarget,
}

/// Failure while parsing reserved information-flow tags.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("{0}")]
pub struct IfcTagError(pub String);

/// Return whether an event contains any reserved information-flow tag.
#[must_use]
pub fn has_ifc_tag(event: &Event) -> bool {
    event.tags.iter().any(|tag| {
        tag.as_slice()
            .first()
            .is_some_and(|name| name.starts_with("ifc"))
    })
}

/// Parse the strict version-one channel publication fence.
///
/// Absence of every reserved tag returns `Ok(None)`. Presence of any `ifc` or
/// `ifc-*` tag opts the complete event into this grammar; partial, unknown,
/// duplicate, noncanonical, or unsupported forms return an error.
pub fn parse_publication_fence(event: &Event) -> Result<Option<IfcPublicationFence>, IfcTagError> {
    if !has_ifc_tag(event) {
        return Ok(None);
    }
    let kind = event_kind_u32(event);
    if !IFC_PUBLICATION_KINDS.contains(&kind) {
        return Err(invalid("event kind is not enabled for fenced publication"));
    }

    let mut common = None;
    let mut destination = None;
    let mut owner_memory = None;
    let mut owner_telemetry = None;
    let mut sources = Vec::new();
    for tag in event.tags.iter() {
        let parts = tag.as_slice();
        let Some(name) = parts.first().map(String::as_str) else {
            continue;
        };
        if !name.starts_with("ifc") {
            continue;
        }
        match name {
            "ifc" => {
                if common.is_some() || parts.len() != 4 || parts[1] != IFC_VERSION {
                    return Err(invalid("invalid or duplicate ifc tag"));
                }
                let community_id = canonical_uuid(&parts[2], "ifc community")?;
                let expires_at = canonical_positive_i64(&parts[3], "ifc expiry")?;
                common = Some((community_id, expires_at));
            }
            "ifc-source" => {
                if parts.len() != 3 {
                    return Err(invalid("ifc-source tag requires channel and generation"));
                }
                sources.push(IfcPolicyRef {
                    channel_id: canonical_uuid(&parts[1], "ifc source channel")?,
                    generation: canonical_positive_i64(&parts[2], "ifc source generation")?,
                });
            }
            "ifc-destination" => {
                if destination.is_some() || parts.len() != 3 {
                    return Err(invalid("invalid or duplicate ifc-destination tag"));
                }
                destination = Some(IfcPolicyRef {
                    channel_id: canonical_uuid(&parts[1], "ifc destination channel")?,
                    generation: canonical_positive_i64(&parts[2], "ifc destination generation")?,
                });
            }
            "ifc-owner-memory" => {
                if owner_memory.is_some() || parts.len() != 2 || !lowercase_hex_32(&parts[1]) {
                    return Err(invalid("invalid or duplicate ifc-owner-memory tag"));
                }
                owner_memory = Some(parts[1].clone());
            }
            "ifc-owner-telemetry" => {
                if owner_telemetry.is_some() || parts.len() != 2 || !lowercase_hex_32(&parts[1]) {
                    return Err(invalid("invalid or duplicate ifc-owner-telemetry tag"));
                }
                owner_telemetry = Some(parts[1].clone());
            }
            _ => return Err(invalid("unknown reserved ifc tag")),
        }
    }

    let (community_id, expires_at) = common.ok_or_else(|| invalid("missing ifc tag"))?;
    if sources.is_empty() || sources.len() > IFC_MAX_SOURCES {
        return Err(invalid("ifc-source count is outside the supported range"));
    }
    let ordered = sources
        .windows(2)
        .all(|pair| pair[0].channel_id.to_string() < pair[1].channel_id.to_string());
    let unique = sources
        .iter()
        .map(|source| source.channel_id)
        .collect::<HashSet<_>>()
        .len()
        == sources.len();
    if !ordered || !unique {
        return Err(invalid(
            "ifc-source tags must be distinct and ordered by channel UUID",
        ));
    }

    let target = if IFC_CHANNEL_PUBLICATION_KINDS.contains(&kind) {
        if owner_memory.is_some() || owner_telemetry.is_some() {
            return Err(invalid("channel publication cannot use an owner target"));
        }
        let destination = destination.ok_or_else(|| invalid("missing ifc-destination tag"))?;
        let h_tags: Vec<_> = event
            .tags
            .iter()
            .map(nostr::Tag::as_slice)
            .filter(|parts| parts.first().is_some_and(|name| name == "h"))
            .collect();
        if h_tags.len() != 1 || h_tags[0].len() != 2 {
            return Err(invalid(
                "fenced channel publication requires exactly one h tag",
            ));
        }
        let event_channel = canonical_uuid(&h_tags[0][1], "h channel")?;
        if event_channel != destination.channel_id {
            return Err(invalid("ifc destination does not match the h channel"));
        }
        IfcPublicationTarget::Channel(destination)
    } else if kind == IFC_OWNER_MEMORY_KIND {
        if destination.is_some() || owner_telemetry.is_some() {
            return Err(invalid("owner memory cannot use another target"));
        }
        let owner_pubkey = owner_memory.ok_or_else(|| invalid("missing ifc-owner-memory tag"))?;
        let mut d_tag = None;
        let mut p_tag = None;
        let mut namespace = None;
        let mut expected_head = None;
        for tag in event.tags.iter() {
            let parts = tag.as_slice();
            let Some(name) = parts.first().map(String::as_str) else {
                return Err(invalid("owner memory contains an empty tag"));
            };
            match name {
                "d" if d_tag.is_none() && parts.len() == 2 && lowercase_hex_32(&parts[1]) => {
                    d_tag = Some(parts[1].clone());
                }
                "p" if p_tag.is_none() && parts.len() == 2 && lowercase_hex_32(&parts[1]) => {
                    p_tag = Some(parts[1].clone());
                }
                "s" if namespace.is_none() && parts.len() == 2 && lowercase_hex_32(&parts[1]) => {
                    namespace = Some(parts[1].clone());
                }
                "expected-revision" if expected_head.is_none() && parts.len() == 2 => {
                    expected_head = Some(parse_memory_expected_head(&parts[1])?);
                }
                "ifc" | "ifc-source" | "ifc-owner-memory" => {}
                _ => return Err(invalid("owner memory contains an unsupported tag")),
            }
        }
        let d_tag = d_tag.ok_or_else(|| invalid("owner memory requires one opaque d tag"))?;
        let p_tag = p_tag.ok_or_else(|| invalid("owner memory requires one owner p tag"))?;
        let namespace =
            namespace.ok_or_else(|| invalid("owner memory requires one opaque s tag"))?;
        let expected_head = expected_head
            .ok_or_else(|| invalid("owner memory requires one expected-revision tag"))?;
        if p_tag != owner_pubkey {
            return Err(invalid("owner memory p tag does not match its fixed owner"));
        }
        IfcPublicationTarget::OwnerMemory {
            owner_pubkey,
            d_tag,
            namespace,
            expected_head,
        }
    } else {
        if destination.is_some() || owner_memory.is_some() {
            return Err(invalid("owner telemetry cannot use another target"));
        }
        if !content_looks_like_nip44(&event.content) {
            return Err(invalid("owner telemetry content must be NIP-44 encrypted"));
        }
        let owner_pubkey =
            owner_telemetry.ok_or_else(|| invalid("missing ifc-owner-telemetry tag"))?;
        let mut p_tag = None;
        let mut agent_tag = None;
        let mut frame_tag = None;
        for tag in event.tags.iter() {
            let parts = tag.as_slice();
            let Some(name) = parts.first().map(String::as_str) else {
                return Err(invalid("owner telemetry contains an empty tag"));
            };
            match name {
                "p" if p_tag.is_none() && parts.len() == 2 && lowercase_hex_32(&parts[1]) => {
                    p_tag = Some(parts[1].clone());
                }
                OBSERVER_AGENT_TAG
                    if agent_tag.is_none() && parts.len() == 2 && lowercase_hex_32(&parts[1]) =>
                {
                    agent_tag = Some(parts[1].clone());
                }
                OBSERVER_FRAME_TAG if frame_tag.is_none() && parts.len() == 2 => {
                    frame_tag = Some(parts[1].clone());
                }
                "ifc" | "ifc-source" | "ifc-owner-telemetry" => {}
                _ => return Err(invalid("owner telemetry contains an unsupported tag")),
            }
        }
        let p_tag = p_tag.ok_or_else(|| invalid("owner telemetry requires one owner p tag"))?;
        let agent_tag =
            agent_tag.ok_or_else(|| invalid("owner telemetry requires one agent tag"))?;
        let frame_tag =
            frame_tag.ok_or_else(|| invalid("owner telemetry requires one frame tag"))?;
        if p_tag != owner_pubkey {
            return Err(invalid(
                "owner telemetry p tag does not match its fixed owner",
            ));
        }
        if agent_tag != event.pubkey.to_hex() || agent_tag == owner_pubkey {
            return Err(invalid(
                "owner telemetry agent tag does not match its author",
            ));
        }
        if frame_tag != OBSERVER_FRAME_TELEMETRY {
            return Err(invalid("owner telemetry frame must be telemetry"));
        }
        IfcPublicationTarget::OwnerTelemetry { owner_pubkey }
    };

    Ok(Some(IfcPublicationFence {
        community_id,
        expires_at,
        sources,
        target,
    }))
}

fn lowercase_hex_32(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn parse_memory_expected_head(value: &str) -> Result<IfcMemoryExpectedHead, IfcTagError> {
    if value == "none" {
        return Ok(IfcMemoryExpectedHead::Absent);
    }
    if !lowercase_hex_32(value) {
        return Err(invalid("owner memory expected revision is not canonical"));
    }
    let bytes = hex::decode(value)
        .map_err(|_| invalid("owner memory expected revision is not canonical"))?;
    let event_id: [u8; 32] = bytes
        .try_into()
        .map_err(|_| invalid("owner memory expected revision is not canonical"))?;
    Ok(IfcMemoryExpectedHead::Event(event_id))
}

fn canonical_uuid(value: &str, field: &str) -> Result<Uuid, IfcTagError> {
    let parsed = Uuid::parse_str(value).map_err(|_| invalid(&format!("{field} is not a UUID")))?;
    if parsed.to_string() != value {
        return Err(invalid(&format!("{field} is not canonical")));
    }
    Ok(parsed)
}

fn canonical_positive_i64(value: &str, field: &str) -> Result<i64, IfcTagError> {
    if value.is_empty()
        || value.starts_with('0')
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid(&format!("{field} is not canonical")));
    }
    value
        .parse::<i64>()
        .ok()
        .filter(|number| *number > 0)
        .ok_or_else(|| invalid(&format!("{field} is outside the supported range")))
}

fn invalid(message: &str) -> IfcTagError {
    IfcTagError(message.to_owned())
}

#[cfg(test)]
mod tests {
    use nostr::{EventBuilder, Keys, Kind, Tag};

    use super::*;

    fn event(kind: u32, tags: &[&[&str]]) -> Event {
        let tags = tags
            .iter()
            .map(|parts| Tag::parse(parts.iter().copied()).expect("valid test tag"))
            .collect::<Vec<_>>();
        EventBuilder::new(Kind::Custom(kind as u16), "test")
            .tags(tags)
            .sign_with_keys(&Keys::generate())
            .expect("sign test event")
    }

    #[test]
    fn absent_reserved_tags_preserve_legacy_events() {
        assert_eq!(
            parse_publication_fence(&event(KIND_STREAM_MESSAGE, &[])),
            Ok(None)
        );
    }

    #[test]
    fn parses_canonical_channel_fence() {
        let source = "11111111-1111-4111-8111-111111111111";
        let destination = "22222222-2222-4222-8222-222222222222";
        let parsed = parse_publication_fence(&event(
            KIND_STREAM_MESSAGE,
            &[
                &["h", destination],
                &[
                    "ifc",
                    "1",
                    "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
                    "1792000000",
                ],
                &["ifc-source", source, "17"],
                &["ifc-destination", destination, "23"],
            ],
        ))
        .expect("parse")
        .expect("fence");
        assert_eq!(parsed.sources[0].generation, 17);
        assert_eq!(
            parsed.target,
            IfcPublicationTarget::Channel(IfcPolicyRef {
                channel_id: Uuid::parse_str(destination).expect("destination UUID"),
                generation: 23,
            })
        );
    }

    #[test]
    fn parses_closed_owner_memory_fence() {
        let owner = "2222222222222222222222222222222222222222222222222222222222222222";
        let d_tag = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let namespace = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
        let parsed = parse_publication_fence(&event(
            KIND_AGENT_ENGRAM,
            &[
                &["d", d_tag],
                &["p", owner],
                &[
                    "ifc",
                    "1",
                    "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
                    "1792000000",
                ],
                &["ifc-source", "11111111-1111-4111-8111-111111111111", "17"],
                &["ifc-owner-memory", owner],
                &["s", namespace],
                &["expected-revision", "none"],
            ],
        ))
        .expect("parse")
        .expect("fence");
        assert_eq!(
            parsed.target,
            IfcPublicationTarget::OwnerMemory {
                owner_pubkey: owner.to_owned(),
                d_tag: d_tag.to_owned(),
                namespace: namespace.to_owned(),
                expected_head: IfcMemoryExpectedHead::Absent,
            }
        );
    }

    #[test]
    fn parses_owner_memory_expected_event_head() {
        let owner = "2222222222222222222222222222222222222222222222222222222222222222";
        let d_tag = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let namespace = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
        let head = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let parsed = parse_publication_fence(&event(
            KIND_AGENT_ENGRAM,
            &[
                &["d", d_tag],
                &["p", owner],
                &[
                    "ifc",
                    "1",
                    "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
                    "1792000000",
                ],
                &["ifc-source", "11111111-1111-4111-8111-111111111111", "17"],
                &["ifc-owner-memory", owner],
                &["s", namespace],
                &["expected-revision", head],
            ],
        ))
        .expect("parse")
        .expect("fence");
        assert_eq!(
            parsed.target,
            IfcPublicationTarget::OwnerMemory {
                owner_pubkey: owner.to_owned(),
                d_tag: d_tag.to_owned(),
                namespace: namespace.to_owned(),
                expected_head: IfcMemoryExpectedHead::Event([0xbb; 32]),
            }
        );
    }

    #[test]
    fn parses_closed_owner_telemetry_fence() {
        let agent = Keys::generate();
        let owner = Keys::generate();
        let encrypted = crate::observer::encrypt_observer_payload(
            &agent,
            &owner.public_key(),
            &serde_json::json!({"type": "tool_result"}),
        )
        .expect("encrypt telemetry");
        let owner_hex = owner.public_key().to_hex();
        let agent_hex = agent.public_key().to_hex();
        let telemetry = EventBuilder::new(Kind::Custom(IFC_OWNER_TELEMETRY_KIND as u16), encrypted)
            .tags([
                Tag::parse(["p", owner_hex.as_str()]).expect("p tag"),
                Tag::parse([OBSERVER_AGENT_TAG, agent_hex.as_str()]).expect("agent tag"),
                Tag::parse([OBSERVER_FRAME_TAG, OBSERVER_FRAME_TELEMETRY]).expect("frame tag"),
                Tag::parse([
                    "ifc",
                    "1",
                    "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
                    "1792000000",
                ])
                .expect("ifc tag"),
                Tag::parse(["ifc-source", "11111111-1111-4111-8111-111111111111", "17"])
                    .expect("source tag"),
                Tag::parse(["ifc-owner-telemetry", owner_hex.as_str()])
                    .expect("owner-telemetry tag"),
            ])
            .sign_with_keys(&agent)
            .expect("sign telemetry");
        let parsed = parse_publication_fence(&telemetry)
            .expect("parse")
            .expect("fence");
        assert_eq!(
            parsed.target,
            IfcPublicationTarget::OwnerTelemetry {
                owner_pubkey: owner_hex
            }
        );
    }

    #[test]
    fn any_reserved_tag_requires_complete_supported_grammar() {
        let owner = "1111111111111111111111111111111111111111111111111111111111111111";
        for tags in [
            vec![vec![
                "ifc-source",
                "11111111-1111-4111-8111-111111111111",
                "1",
            ]],
            vec![vec!["ifc-owner-memory", owner]],
            vec![vec!["ifc-owner-telemetry", owner]],
            vec![vec![
                "ifc",
                "2",
                "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
                "10",
            ]],
        ] {
            let borrowed = tags
                .iter()
                .map(|parts| parts.as_slice())
                .collect::<Vec<_>>();
            assert!(parse_publication_fence(&event(KIND_STREAM_MESSAGE, &borrowed)).is_err());
        }
    }

    #[test]
    fn malformed_owner_memory_fences_fail_closed() {
        let owner = "2222222222222222222222222222222222222222222222222222222222222222";
        let other = "3333333333333333333333333333333333333333333333333333333333333333";
        let d_tag = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let common = [
            "ifc",
            "1",
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            "1792000000",
        ];
        let source = ["ifc-source", "11111111-1111-4111-8111-111111111111", "17"];
        let expected = ["expected-revision", "none"];
        let namespace = [
            "s",
            "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
        ];
        for tags in [
            vec![&["d", d_tag][..], &["p", owner], &common, &source],
            vec![
                &["d", d_tag],
                &["p", other],
                &common,
                &source,
                &["ifc-owner-memory", owner],
                &namespace,
                &expected,
            ],
            vec![
                &["d", d_tag],
                &["p", owner],
                &["h", "11111111-1111-4111-8111-111111111111"],
                &common,
                &source,
                &["ifc-owner-memory", owner],
                &namespace,
                &expected,
            ],
            vec![
                &["d", d_tag],
                &["p", owner],
                &common,
                &source,
                &["ifc-owner-memory", owner],
                &namespace,
                &[
                    "expected-revision",
                    "ABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCD",
                ],
            ],
            vec![
                &["d", d_tag],
                &["p", owner],
                &common,
                &source,
                &["ifc-owner-memory", owner],
                &namespace,
                &["expected-revision", "abcd"],
            ],
            vec![
                &["d", d_tag],
                &["p", owner],
                &common,
                &source,
                &["ifc-owner-memory", owner],
                &namespace,
                &expected,
                &expected,
            ],
            vec![
                &["d", d_tag],
                &["p", owner],
                &common,
                &source,
                &["ifc-owner-memory", owner],
                &namespace,
                &["expected-revision", "none", "extra"],
            ],
            vec![
                &["d", d_tag],
                &["p", owner],
                &common,
                &source,
                &["ifc-owner-memory", owner],
                &expected,
            ],
            vec![
                &["d", d_tag],
                &["p", owner],
                &common,
                &source,
                &["ifc-owner-memory", owner],
                &namespace,
                &namespace,
                &expected,
            ],
            vec![
                &["d", d_tag],
                &["p", owner],
                &common,
                &source,
                &["ifc-owner-memory", owner],
                &[
                    "s",
                    "CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC",
                ],
                &expected,
            ],
            vec![
                &["d", d_tag],
                &["p", owner],
                &common,
                &source,
                &["ifc-owner-memory", owner],
                &["memory-scope", namespace[1]],
                &expected,
            ],
        ] {
            assert!(parse_publication_fence(&event(KIND_AGENT_ENGRAM, &tags)).is_err());
        }
    }

    #[test]
    fn malformed_owner_telemetry_fences_fail_closed() {
        let agent = Keys::generate();
        let owner = Keys::generate();
        let other = Keys::generate();
        let encrypted = crate::observer::encrypt_observer_payload(
            &agent,
            &owner.public_key(),
            &serde_json::json!({"type": "tool_result"}),
        )
        .expect("encrypt telemetry");
        let agent_hex = agent.public_key().to_hex();
        let owner_hex = owner.public_key().to_hex();
        let other_hex = other.public_key().to_hex();
        let common = [
            "ifc",
            "1",
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            "1792000000",
        ];
        let source = ["ifc-source", "11111111-1111-4111-8111-111111111111", "17"];
        for tags in [
            vec![
                vec!["p", owner_hex.as_str()],
                vec![OBSERVER_AGENT_TAG, agent_hex.as_str()],
                vec![OBSERVER_FRAME_TAG, OBSERVER_FRAME_TELEMETRY],
                common.to_vec(),
                source.to_vec(),
            ],
            vec![
                vec!["p", other_hex.as_str()],
                vec![OBSERVER_AGENT_TAG, agent_hex.as_str()],
                vec![OBSERVER_FRAME_TAG, OBSERVER_FRAME_TELEMETRY],
                common.to_vec(),
                source.to_vec(),
                vec!["ifc-owner-telemetry", owner_hex.as_str()],
            ],
            vec![
                vec!["p", owner_hex.as_str()],
                vec![OBSERVER_AGENT_TAG, other_hex.as_str()],
                vec![OBSERVER_FRAME_TAG, OBSERVER_FRAME_TELEMETRY],
                common.to_vec(),
                source.to_vec(),
                vec!["ifc-owner-telemetry", owner_hex.as_str()],
            ],
            vec![
                vec!["p", owner_hex.as_str()],
                vec![OBSERVER_AGENT_TAG, agent_hex.as_str()],
                vec![OBSERVER_FRAME_TAG, crate::observer::OBSERVER_FRAME_CONTROL],
                common.to_vec(),
                source.to_vec(),
                vec!["ifc-owner-telemetry", owner_hex.as_str()],
            ],
            vec![
                vec!["p", owner_hex.as_str()],
                vec![OBSERVER_AGENT_TAG, agent_hex.as_str()],
                vec![OBSERVER_FRAME_TAG, OBSERVER_FRAME_TELEMETRY],
                vec![
                    "d",
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                ],
                common.to_vec(),
                source.to_vec(),
                vec!["ifc-owner-telemetry", owner_hex.as_str()],
            ],
            vec![
                vec!["p", owner_hex.as_str()],
                vec![OBSERVER_AGENT_TAG, agent_hex.as_str()],
                vec![OBSERVER_FRAME_TAG, OBSERVER_FRAME_TELEMETRY],
                common.to_vec(),
                source.to_vec(),
                vec!["ifc-owner-memory", owner_hex.as_str()],
            ],
        ] {
            let parsed_tags = tags
                .iter()
                .map(|parts| Tag::parse(parts.clone()).expect("test tag"))
                .collect::<Vec<_>>();
            let candidate = EventBuilder::new(
                Kind::Custom(IFC_OWNER_TELEMETRY_KIND as u16),
                encrypted.clone(),
            )
            .tags(parsed_tags)
            .sign_with_keys(&agent)
            .expect("sign malformed telemetry");
            assert!(parse_publication_fence(&candidate).is_err());
        }
    }
}
