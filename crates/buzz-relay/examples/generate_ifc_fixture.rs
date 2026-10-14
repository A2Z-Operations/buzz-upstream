use base64::Engine;
use hmac::{Hmac, Mac};
use nostr::nips::nip44::v2;
use nostr::secp256k1::rand::RngCore;
use nostr::{EventBuilder, Keys, Kind, Tag, Timestamp};
use serde_json::json;
use sha2::{Digest, Sha256};

struct FixedNonce(u8);

impl RngCore for FixedNonce {
    fn next_u32(&mut self) -> u32 {
        u32::from_le_bytes([self.0; 4])
    }

    fn next_u64(&mut self) -> u64 {
        u64::from_le_bytes([self.0; 8])
    }

    fn fill_bytes(&mut self, destination: &mut [u8]) {
        destination.fill(self.0);
    }

    fn try_fill_bytes(
        &mut self,
        destination: &mut [u8],
    ) -> Result<(), nostr::secp256k1::rand::Error> {
        self.fill_bytes(destination);
        Ok(())
    }
}

impl nostr::secp256k1::rand::CryptoRng for FixedNonce {}

fn main() -> anyhow::Result<()> {
    let relay_secret = "0000000000000000000000000000000000000000000000000000000000000001";
    let human_secret = "0000000000000000000000000000000000000000000000000000000000000002";
    let broker_secret = "0000000000000000000000000000000000000000000000000000000000000003";
    let relay = Keys::parse(relay_secret)?;
    let human = Keys::parse(human_secret)?;
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
        .build(human.public_key())
        .sign_with_ctx(nostr::SECP256K1, &mut FixedNonce(0x21), &human)?;
    let digest = hex::encode(Sha256::digest(request_body.as_bytes()));
    let mut members = [human.public_key().to_hex(), broker.public_key().to_hex()];
    members.sort();
    let content = json!({
        "version": 1,
        "community_id": community,
        "requester": broker.public_key().to_hex(),
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
            Tag::parse(["p", broker.public_key().to_hex().as_str()])?,
            Tag::parse(["h", source])?,
        ])
        .custom_created_at(Timestamp::from(1_791_999_800u64))
        .build(relay.public_key())
        .sign_with_ctx(nostr::SECP256K1, &mut FixedNonce(0x22), &relay)?;
    let publication = EventBuilder::new(Kind::Custom(40002), "fixture remote-agent output")
        .tags([
            Tag::parse(["h", destination])?,
            Tag::parse(["ifc", "1", community, "1792000000"])?,
            Tag::parse(["ifc-source", source, "17"])?,
            Tag::parse(["ifc-destination", destination, "23"])?,
        ])
        .custom_created_at(Timestamp::from(1_791_999_900u64))
        .build(broker.public_key())
        .sign_with_ctx(nostr::SECP256K1, &mut FixedNonce(0x23), &broker)?;
    let memory_slug = "mem/fixture";
    let memory_key = v2::ConversationKey::derive(broker.secret_key(), &broker.public_key())?;
    let mut audience_principals = [human.public_key().to_hex(), broker.public_key().to_hex()];
    audience_principals.sort();
    let audience_bytes = serde_json::to_vec(&json!([
        community,
        broker.public_key().to_hex(),
        human.public_key().to_hex(),
        "restricted",
        audience_principals
    ]))?;
    let mut scope = <Hmac<Sha256> as hmac::digest::KeyInit>::new_from_slice(memory_key.as_bytes())?;
    scope.update(b"agent-memory/ifc/v1/scope");
    scope.update(&[0]);
    scope.update(&audience_bytes);
    let memory_namespace = hex::encode(scope.finalize().into_bytes());
    let mut memory_address =
        <Hmac<Sha256> as hmac::digest::KeyInit>::new_from_slice(memory_key.as_bytes())?;
    memory_address.update(b"agent-memory/v1/d-tag");
    memory_address.update(&[0]);
    memory_address.update(memory_slug.as_bytes());
    let memory_d = hex::encode(memory_address.finalize().into_bytes());
    let memory_plaintext = format!(
        "{{\"slug\":\"{memory_slug}\",\"value\":\"fixture protected memory\",\"ifc\":{{\"version\":1,\"community_id\":\"{community}\",\"sources\":[{{\"channel_id\":\"{source}\",\"generation\":\"17\"}}],\"provenance\":[{{\"event_id\":\"{}\"}}]}}}}",
        resource.id.to_hex()
    );
    let memory_payload = v2::encrypt_to_bytes_with_rng(
        &mut FixedNonce(0x44),
        &memory_key,
        memory_plaintext.as_bytes(),
    )?;
    let memory_ciphertext = base64::engine::general_purpose::STANDARD.encode(memory_payload);
    let owner = human.public_key().to_hex();
    let broker_pubkey = broker.public_key().to_hex();
    let managed_memory = EventBuilder::new(Kind::Custom(30174), memory_ciphertext)
        .tags([
            Tag::parse(["d", memory_d.as_str()])?,
            Tag::parse(["p", broker_pubkey.as_str()])?,
            Tag::parse(["ifc", "1", community, "1792000000"])?,
            Tag::parse(["ifc-source", source, "17"])?,
            Tag::parse(["ifc-managed-memory", broker_pubkey.as_str()])?,
            Tag::parse(["s", memory_namespace.as_str()])?,
            Tag::parse(["expected-revision", "none"])?,
        ])
        .allow_self_tagging()
        .custom_created_at(Timestamp::from(1_791_999_920u64))
        .build(broker.public_key())
        .sign_with_ctx(nostr::SECP256K1, &mut FixedNonce(0x24), &broker)?;
    let telemetry_plaintext = r#"{"type":"tool_result","rawInput":"fixture tool arguments","resultText":"fixture tool result"}"#;
    let telemetry_key = v2::ConversationKey::derive(broker.secret_key(), &human.public_key())?;
    let telemetry_payload = v2::encrypt_to_bytes_with_rng(
        &mut FixedNonce(0x45),
        &telemetry_key,
        telemetry_plaintext.as_bytes(),
    )?;
    let telemetry_ciphertext = base64::engine::general_purpose::STANDARD.encode(telemetry_payload);
    let owner_telemetry = EventBuilder::new(Kind::Custom(24200), telemetry_ciphertext)
        .tags([
            Tag::parse(["p", owner.as_str()])?,
            Tag::parse(["agent", broker_pubkey.as_str()])?,
            Tag::parse(["frame", "telemetry"])?,
            Tag::parse(["ifc", "1", community, "1792000000"])?,
            Tag::parse(["ifc-source", source, "17"])?,
            Tag::parse(["ifc-owner-telemetry", owner.as_str()])?,
        ])
        .custom_created_at(Timestamp::from(1_791_999_930u64))
        .build(broker.public_key())
        .sign_with_ctx(nostr::SECP256K1, &mut FixedNonce(0x25), &broker)?;
    let fixture = json!({
        "contract": "buzz-ifc-v1",
        "test_keys": {
            "relay_secret_hex": relay_secret,
            "relay_pubkey": relay.public_key().to_hex(),
            "human_secret_hex": human_secret,
            "human_pubkey": human.public_key().to_hex(),
            "broker_secret_hex": broker_secret,
            "broker_pubkey": broker.public_key().to_hex(),
            "query_requester_pubkey": broker.public_key().to_hex()
        },
        "request_body_utf8": request_body,
        "request_sha256": digest,
        "response": {"ifc": 1, "attestation": attestation},
        "channel_publication_event": publication,
        "managed_memory": {
            "event": managed_memory,
            "canonical_slug": memory_slug,
            "d_tag": memory_d,
            "namespace": memory_namespace,
            "recipient": broker_pubkey,
            "saved_human_owner": owner,
            "scope_canonical_json_utf8": String::from_utf8(audience_bytes)?,
            "plaintext_utf8": memory_plaintext,
            "application_metadata_status": "illustrative until the exact Kotlin protected version-two object is supplied"
        },
        "owner_telemetry": {
            "event": owner_telemetry,
            "plaintext_utf8": telemetry_plaintext,
            "application_payload_status": "deterministic wire example; backend preserves its existing encrypted telemetry envelope"
        }
    });
    println!("{}", serde_json::to_string_pretty(&fixture)?);
    Ok(())
}
