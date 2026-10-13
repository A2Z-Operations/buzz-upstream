use super::*;
use nostr::{EventBuilder, Keys, Kind, Tag};

const AGENT: &str = "test-agent";
const RELAY: &str = "wss://relay.invalid";
fn private_directory() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    dir
}

fn open(path: &Path) -> EventQueue {
    EventQueue::new(DedupMode::Queue)
        .with_delivery_journal(path, AGENT, RELAY)
        .unwrap()
}
fn event(channel_id: Uuid, content: &str) -> QueuedEvent {
    QueuedEvent {
        channel_id,
        scope: channel_id.into_scope(),
        event: EventBuilder::new(Kind::Custom(9), content)
            .tags([Tag::parse(["h", &channel_id.to_string()]).unwrap()])
            .sign_with_keys(&Keys::generate())
            .unwrap(),
        received_at: Instant::now(),
        prompt_tag: "mention".into(),
        edit: None,
    }
}
fn restore(queue: &mut EventQueue, channels: &HashSet<Uuid>) {
    for stored in queue.restore_events(channels) {
        assert!(queue.push(QueuedEvent {
            channel_id: stored.channel_id,
            scope: stored.channel_id.into_scope(),
            event: stored.event,
            received_at: Instant::now(),
            prompt_tag: "mention".into(),
            edit: None
        }));
    }
}

#[test]
fn controlled_restart_restores_pending_and_deduplicates_completed_relay_overlap() {
    let dir = private_directory();
    let path = dir.path().join("journal.json");
    let channel = Uuid::new_v4();
    let completed = event(channel, "completed");
    let pending = event(channel, "pending");
    let mut queue = open(&path);
    assert!(queue.push(completed.clone()));
    queue.flush_next().unwrap();
    queue.mark_complete(channel);
    assert!(queue.push(pending.clone()));
    drop(queue);
    let mut replacement = open(&path);
    assert!(
        !replacement.push(completed),
        "controlled restart's relay backfill must not replay a completed event"
    );
    restore(&mut replacement, &HashSet::from([channel]));
    assert!(
        !replacement.push(pending.clone()),
        "live relay overlap must not duplicate restored work"
    );
    let batch = replacement.flush_next().unwrap();
    assert_eq!(batch.events.len(), 1);
    assert_eq!(batch.events[0].event.id, pending.event.id);
    replacement.mark_complete(channel);
    drop(replacement);
    assert!(!open(&path).has_restore_pending());
}

#[test]
fn unresolved_restored_authority_is_retained_without_dispatch() {
    let dir = private_directory();
    let path = dir.path().join("journal.json");
    let channel = Uuid::new_v4();
    let mut queue = open(&path);
    queue.push(event(channel, "accepted"));
    drop(queue);
    let mut replacement = open(&path);
    assert!(replacement.restore_events(&HashSet::new()).is_empty());
    assert_eq!(
        replacement.restore_events(&HashSet::from([channel])).len(),
        1
    );
    assert!(
        replacement.flush_next().is_none(),
        "restored work must pass admission before dispatch"
    );
    // Simulate author lookup unavailable: do not admit the candidate.
    replacement.checkpoint();
    drop(replacement);
    assert!(open(&path).has_restore_pending());
}

#[test]
fn journal_refuses_wrong_owner_corruption_and_concurrent_writer() {
    let dir = private_directory();
    let path = dir.path().join("journal.json");
    let queue = open(&path);
    assert!(DeliveryJournal::open(&path, AGENT, RELAY).is_err());
    drop(queue);
    assert!(DeliveryJournal::open(&path, "another-agent", RELAY).is_err());
    assert!(DeliveryJournal::open(&path, AGENT, "wss://another-relay.invalid").is_err());
    std::fs::write(&path, b"invalid json").unwrap();
    assert!(DeliveryJournal::open(&path, AGENT, RELAY).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"invalid json");
}

#[test]
fn completed_retention_covers_replay_floor_and_fails_closed_at_capacity() {
    let dir = private_directory();
    let mut queue = open(&dir.path().join("journal.json"));
    let now = nostr::Timestamp::now().as_secs();
    let journal = queue.journal.as_mut().unwrap();
    for index in 0..MAX_RECORDS {
        journal
            .snapshot
            .completed
            .insert(format!("{index:064x}"), now);
    }
    assert!(!queue.push(event(Uuid::new_v4(), "over capacity")));
    assert!(queue.flush_next().is_none());
    assert!(queue.check_persistence().is_err());
    assert_eq!(
        queue.journal.as_ref().unwrap().snapshot.completed.len(),
        MAX_RECORDS
    );
    assert_eq!(COMPLETED_RETENTION_SECS, 905);
}

#[test]
fn journal_write_failure_stops_admission_and_dispatch() {
    let dir = private_directory();
    let path = dir.path().join("journal.json");
    let mut queue = open(&path);
    // A directory cannot be atomically replaced with our JSON snapshot.
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(!queue.push(event(Uuid::new_v4(), "unaccepted")));
    assert!(queue.check_persistence().is_err());
    assert!(queue.flush_next().is_none());
}

#[cfg(unix)]
#[test]
fn journal_is_private_and_refuses_symlinks() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let dir = private_directory();
    let path = dir.path().join("journal.json");
    drop(open(&path));
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let link = dir.path().join("linked.json");
    symlink(&path, &link).unwrap();
    assert!(DeliveryJournal::open(&link, AGENT, RELAY).is_err());
}

#[test]
fn journal_rejects_signed_event_channel_mismatch() {
    let dir = private_directory();
    let path = dir.path().join("journal.json");
    let mut queue = open(&path);
    queue.push(event(Uuid::new_v4(), "accepted"));
    drop(queue);
    let mut data: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let entry = data["pending"]
        .as_object_mut()
        .unwrap()
        .values_mut()
        .next()
        .unwrap();
    entry["channel_id"] = Uuid::new_v4().to_string().into();
    std::fs::write(&path, serde_json::to_vec(&data).unwrap()).unwrap();
    assert!(DeliveryJournal::open(&path, AGENT, RELAY).is_err());
}

#[cfg(unix)]
#[test]
#[ignore = "subprocess fixture invoked by forced_kill_recovers_accepted_in_flight_and_steered_work"]
fn journal_crash_child() {
    let path = PathBuf::from(std::env::var_os("JOURNAL_FIXTURE_PATH").unwrap());
    let channel = Uuid::parse_str(&std::env::var("JOURNAL_FIXTURE_CHANNEL").unwrap()).unwrap();
    let mut queue = open(&path);
    queue.push(event(channel, "completed"));
    queue.flush_next().unwrap();
    queue.mark_complete(channel);
    queue.push(event(channel, "original"));
    queue.flush_next().unwrap();
    let steer = event(channel, "follow-up");
    let id = steer.event.id.to_hex();
    queue.push(steer);
    queue.mark_native_steer_pending(&channel.into_scope(), &id);
    queue.acknowledge_native_steer(&channel.into_scope(), &id);
    std::fs::write(path.with_extension("ready"), b"ready").unwrap();
    loop {
        std::thread::park();
    }
}

#[cfg(unix)]
#[test]
fn forced_kill_recovers_accepted_in_flight_and_steered_work() {
    let dir = private_directory();
    let path = dir.path().join("journal.json");
    let channel = Uuid::new_v4();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "queue::delivery_journal::tests::journal_crash_child",
            "--ignored",
        ])
        .env("JOURNAL_FIXTURE_PATH", &path)
        .env("JOURNAL_FIXTURE_CHANNEL", channel.to_string())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let ready = path.with_extension("ready");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready.exists() && Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let was_ready = ready.exists();
    child.kill().ok();
    child.wait().unwrap();
    assert!(was_ready, "fixture must admit work before it is killed");
    let mut replacement = open(&path);
    restore(&mut replacement, &HashSet::from([channel]));
    let batch = replacement.flush_next().unwrap();
    let bodies: HashSet<_> = batch
        .events
        .iter()
        .map(|event| event.event.content.as_str())
        .collect();
    assert_eq!(bodies, HashSet::from(["original", "follow-up"]));
    replacement.mark_complete(channel);
    assert!(replacement.flush_next().is_none());
    drop(replacement);
    assert!(!open(&path).has_restore_pending());
}

#[test]
fn deadline_expiry_preserves_original_before_any_replacement_can_dispatch() {
    for use_flush in [true, false] {
        let dir = private_directory();
        let path = dir.path().join("journal.json");
        let channel = Uuid::new_v4();
        let mut queue = open(&path);
        queue.push(event(channel, "original"));
        queue.flush_next().unwrap();
        queue.push(event(channel, "follow-up"));
        queue
            .in_flight_deadlines
            .insert(channel.into_scope(), Instant::now());
        if use_flush {
            assert!(queue.flush_next().is_none());
        } else {
            assert!(!queue.has_flushable_work());
        }
        assert!(queue.check_persistence().is_err());
        drop(queue);
        let mut replacement = open(&path);
        restore(&mut replacement, &HashSet::from([channel]));
        assert_eq!(replacement.flush_next().unwrap().events.len(), 2);
    }
}

#[test]
fn accepted_event_older_than_relay_window_remains_recoverable() {
    let dir = private_directory();
    let path = dir.path().join("journal.json");
    let channel = Uuid::new_v4();
    let mut old = event(channel, "old accepted work");
    old.event = EventBuilder::new(Kind::Custom(9), "old accepted work")
        .tags([Tag::parse(["h", &channel.to_string()]).unwrap()])
        .custom_created_at(nostr::Timestamp::from(
            nostr::Timestamp::now().as_secs() - 3600,
        ))
        .sign_with_keys(&Keys::generate())
        .unwrap();
    let mut queue = open(&path);
    queue.push(old);
    drop(queue);
    let mut replacement = open(&path);
    restore(&mut replacement, &HashSet::from([channel]));
    assert_eq!(
        replacement.flush_next().unwrap().events[0].event.content,
        "old accepted work"
    );
}

#[test]
fn same_second_follow_ups_keep_their_admission_order_across_restart() {
    let dir = private_directory();
    let path = dir.path().join("journal.json");
    let channel = Uuid::new_v4();
    let mut queue = open(&path);
    let created_at = nostr::Timestamp::now();
    let mut expected = Vec::new();
    for index in 0..16 {
        let mut queued = event(channel, "");
        queued.event = EventBuilder::new(Kind::Custom(9), index.to_string())
            .tags([Tag::parse(["h", &channel.to_string()]).unwrap()])
            .custom_created_at(created_at)
            .sign_with_keys(&Keys::generate())
            .unwrap();
        expected.push(queued.event.id);
        queue.push(queued);
    }
    drop(queue);
    let mut replacement = open(&path);
    restore(&mut replacement, &HashSet::from([channel]));
    assert_eq!(
        replacement
            .flush_next()
            .unwrap()
            .events
            .iter()
            .map(|event| event.event.id)
            .collect::<Vec<_>>(),
        expected
    );
}

#[cfg(unix)]
#[tokio::test]
async fn graceful_shutdown_settles_original_and_follow_up_in_either_ack_order() {
    use crate::pool::{PromptOutcome, PromptResult, PromptSource};
    for ack_first in [true, false] {
        let dir = private_directory();
        let path = dir.path().join("journal.json");
        let channel = Uuid::new_v4();
        let mut queue = open(&path);
        queue.push(event(channel, "original"));
        queue.flush_next().unwrap();
        let follow_up = event(channel, "follow-up");
        let id = follow_up.event.id.to_hex();
        queue.push(follow_up);
        queue.mark_native_steer_pending(&channel.into_scope(), &id);
        if ack_first {
            queue.acknowledge_native_steer(&channel.into_scope(), &id);
        }
        let mut result = PromptResult {
            agent: crate::error_outcome_emission_tests::dummy_agent(0).await,
            source: PromptSource::Channel(channel.into_scope()),
            turn_id: "shutdown-test".into(),
            outcome: PromptOutcome::Ok(crate::acp::StopReason::EndTurn),
            batch: None,
        };
        queue.settle_shutdown_result(&result);
        if !ack_first {
            queue.acknowledge_native_steer(&channel.into_scope(), &id);
        }
        result.agent.acp.shutdown().await;
        drop(queue);
        assert!(
            !open(&path).has_restore_pending(),
            "drain completion must be committed before replacement starts"
        );
    }
}

#[tokio::test]
async fn old_restored_request_requires_current_author_and_subscription_admission() {
    use crate::{
        authorize_normal_listener_event, AuthorizedNormalListenerEvent, InboundAuthorGate,
        OwnerCache,
    };
    let dir = private_directory();
    let path = dir.path().join("journal.json");
    let channel = Uuid::new_v4();
    let mut queued = event(channel, "old accepted");
    queued.event = EventBuilder::new(Kind::Custom(9), "old accepted")
        .tags([Tag::parse(["h", &channel.to_string()]).unwrap()])
        .custom_created_at(nostr::Timestamp::from(
            nostr::Timestamp::now().as_secs() - 3600,
        ))
        .sign_with_keys(&Keys::generate())
        .unwrap();
    let author = queued.event.pubkey.to_hex();
    let mut queue = open(&path);
    queue.push(queued);
    drop(queue);
    let mut replacement = open(&path);
    let restored = replacement
        .restore_events(&HashSet::from([channel]))
        .pop_front()
        .unwrap();
    let rest = crate::relay::RestClient {
        http: reqwest::Client::builder()
            .timeout(Duration::from_millis(100))
            .build()
            .unwrap(),
        base_url: "http://127.0.0.1:9".into(),
        keys: Keys::generate(),
        auth_tag_json: None,
    };
    let channel_info = crate::pool::ChannelInfoResolver::new(
        HashMap::from([(
            channel,
            crate::relay::ChannelInfo {
                name: "fixture".into(),
                channel_type: "stream".into(),
                description: None,
            },
        )]),
        rest.clone(),
    );
    let agent = Keys::generate().public_key().to_hex();
    let mut gate = InboundAuthorGate::connect(&rest, &agent, "journal-test").await;
    let other_owner = OwnerCache::new(Some(Keys::generate().public_key().to_hex()));
    other_owner.cache_sibling(author.clone(), false);
    assert!(authorize_normal_listener_event(
        &mut gate,
        restored.clone(),
        &crate::config::RespondTo::OwnerOnly,
        &HashSet::new(),
        &other_owner,
        &channel_info,
        &rest
    )
    .await
    .is_none());
    assert!(
        replacement.has_restore_pending(),
        "denied or unresolved authority must not erase the record"
    );
    let owner = OwnerCache::new(Some(author));
    let authorized = authorize_normal_listener_event(
        &mut gate,
        restored,
        &crate::config::RespondTo::OwnerOnly,
        &HashSet::new(),
        &owner,
        &channel_info,
        &rest,
    )
    .await
    .unwrap();
    let rule =
        serde_json::from_value(serde_json::json!({"name":"fixture","channels":"all","kinds":[9]}))
            .unwrap();
    let ingress = AuthorizedNormalListenerEvent(authorized)
        .match_subscription(&[rule], &agent)
        .await
        .unwrap();
    assert!(
        ingress
            .push(&mut replacement, channel.into_scope(), false)
            .accepted
    );
    assert_eq!(
        replacement.flush_next().unwrap().events[0].event.content,
        "old accepted"
    );
}
