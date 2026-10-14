use nostr::{EventBuilder, Keys, Kind, Tag, Timestamp};
use serde_json::json;
use sha2::{Digest, Sha256};

fn main() -> anyhow::Result<()> {
    let relay_secret = "0000000000000000000000000000000000000000000000000000000000000001";
    let requester_secret = "0000000000000000000000000000000000000000000000000000000000000002";
    let broker_secret = "0000000000000000000000000000000000000000000000000000000000000003";
    let relay = Keys::parse(relay_secret)?;
    let requester = Keys::parse(requester_secret)?;
    let broker = Keys::parse(broker_secret)?;
    let community = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let source = "11111111-1111-4111-8111-111111111111";
    let destination = "22222222-2222-4222-8222-222222222222";
    let nonce = "0123456789abcdef0123456789abcdef";
    let request_body = format!(
        "[{{\"kinds\":[9,40002,40100],\"#h\":[\"{source}\"],\"limit\":13,\"ifc\":{{\"version\":1,\"nonce\":\"{nonce}\",\"expected_generation\":\"17\"}}}}]"
    );
    let resource = EventBuilder::new(Kind::Custom(40002), "fixture source message")
        .tags([Tag::parse(["h", source])?])
        .custom_created_at(Timestamp::from(1_791_999_700u64))
        .sign_with_keys(&requester)?;
    let digest = hex::encode(Sha256::digest(request_body.as_bytes()));
    let mut members = [
        requester.public_key().to_hex(),
        broker.public_key().to_hex(),
    ];
    members.sort();
    let content = json!({
        "version": 1,
        "community_id": community,
        "requester": requester.public_key().to_hex(),
        "nonce": nonce,
        "request_sha256": digest,
        "policy": {
            "channel_id": source,
            "generation": "17",
            "channel_type": "dm",
            "visibility": "private",
            "history_access": "all_current_members",
            "archived": false,
            "members": members
        },
        "resources": [{
            "event": resource,
            "effective_generation": "17",
            "origin_generation": null
        }]
    });
    let attestation = EventBuilder::new(Kind::Custom(45900), serde_json::to_string(&content)?)
        .tags([
            Tag::parse(["p", requester.public_key().to_hex().as_str()])?,
            Tag::parse(["h", source])?,
        ])
        .custom_created_at(Timestamp::from(1_791_999_800u64))
        .sign_with_keys(&relay)?;
    let publication = EventBuilder::new(Kind::Custom(40002), "fixture remote-agent output")
        .tags([
            Tag::parse(["h", destination])?,
            Tag::parse(["ifc", "1", community, "1792000000"])?,
            Tag::parse(["ifc-source", source, "17"])?,
            Tag::parse(["ifc-destination", destination, "23"])?,
        ])
        .custom_created_at(Timestamp::from(1_791_999_900u64))
        .sign_with_keys(&broker)?;
    let fixture = json!({
        "contract": "buzz-ifc-v1",
        "test_keys": {
            "relay_secret_hex": relay_secret,
            "relay_pubkey": relay.public_key().to_hex(),
            "requester_secret_hex": requester_secret,
            "requester_pubkey": requester.public_key().to_hex(),
            "broker_secret_hex": broker_secret,
            "broker_pubkey": broker.public_key().to_hex()
        },
        "request_body_utf8": request_body,
        "request_sha256": digest,
        "response": {"ifc": 1, "attestation": attestation},
        "channel_publication_event": publication,
        "owner_memory": {"enabled": false, "reason": "awaiting the backend's existing memory address grammar"}
    });
    println!("{}", serde_json::to_string_pretty(&fixture)?);
    Ok(())
}
