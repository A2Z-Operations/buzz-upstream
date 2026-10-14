//! Shared version-one remote-agent information-flow fence grammar.

use std::collections::HashSet;

use nostr::Event;
use thiserror::Error;
use uuid::Uuid;

use crate::kind::{event_kind_u32, KIND_CANVAS, KIND_STREAM_MESSAGE, KIND_STREAM_MESSAGE_V2};

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

/// One channel policy coordinate asserted by the trusted broker.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct IfcPolicyRef {
    /// Channel whose canonical policy is referenced.
    pub channel_id: Uuid,
    /// Positive monotone generation encoded canonically on the wire.
    pub generation: i64,
}

/// Strictly parsed version-one channel publication fence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IfcPublicationFence {
    /// Host-bound community asserted in the signed event.
    pub community_id: Uuid,
    /// Fence expiry as Unix seconds.
    pub expires_at: i64,
    /// Sorted, distinct source policy coordinates.
    pub sources: Vec<IfcPolicyRef>,
    /// Destination channel policy coordinate.
    pub destination: IfcPolicyRef,
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
    if !IFC_CHANNEL_PUBLICATION_KINDS.contains(&kind) {
        return Err(invalid("event kind is not enabled for fenced publication"));
    }

    let mut common = None;
    let mut destination = None;
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
            _ => return Err(invalid("unknown reserved ifc tag")),
        }
    }

    let (community_id, expires_at) = common.ok_or_else(|| invalid("missing ifc tag"))?;
    let destination = destination.ok_or_else(|| invalid("missing ifc-destination tag"))?;
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

    let h_tags: Vec<_> = event
        .tags
        .iter()
        .map(nostr::Tag::as_slice)
        .filter(|parts| parts.first().is_some_and(|name| name == "h"))
        .collect();
    if h_tags.len() != 1 || h_tags[0].len() != 2 {
        return Err(invalid("fenced publication requires exactly one h tag"));
    }
    let event_channel = canonical_uuid(&h_tags[0][1], "h channel")?;
    if event_channel != destination.channel_id {
        return Err(invalid("ifc destination does not match the h channel"));
    }

    Ok(Some(IfcPublicationFence {
        community_id,
        expires_at,
        sources,
        destination,
    }))
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
        assert_eq!(parsed.destination.generation, 23);
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
}
