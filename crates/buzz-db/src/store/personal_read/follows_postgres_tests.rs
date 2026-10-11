//! Who follows a thread, as recorded by ingest, and what following shows.
use super::{
    postgres_tests::{fixture, insert_reply, start_before_everything},
    *,
};
use crate::Db;
use buzz_core::{channel::MemberRole, CommunityId};
use nostr::{EventBuilder, Keys, Kind, Tag};
use sqlx::PgPool;
use uuid::Uuid;

fn message(author: &Keys, tags: Vec<Tag>) -> nostr::Event {
    EventBuilder::new(Kind::Custom(9), Uuid::new_v4().to_string())
        .tags(tags)
        .sign_with_keys(author)
        .unwrap()
}

async fn post(
    db: &Db,
    community: CommunityId,
    channel: Uuid,
    author: &Keys,
    tags: Vec<Tag>,
) -> nostr::Event {
    let event = message(author, tags);
    db.insert_event(community, &event, Some(channel))
        .await
        .unwrap();
    event
}

async fn reply(
    db: &Db,
    community: CommunityId,
    channel: Uuid,
    root: &nostr::Event,
    author: &Keys,
    tags: Vec<Tag>,
) -> nostr::Event {
    let event = message(author, tags);
    insert_reply(db, community, channel, root, &event).await;
    event
}

fn mention(who: &Keys) -> Vec<Tag> {
    vec![Tag::public_key(who.public_key())]
}

async fn member(db: &Db, pool: &PgPool, community: CommunityId, channel: Uuid) -> Keys {
    let keys = Keys::generate();
    db.add_member(
        community,
        channel,
        &keys.public_key().to_bytes(),
        MemberRole::Member,
        None,
    )
    .await
    .unwrap();
    start_before_everything(pool, community, &keys.public_key()).await;
    keys
}

async fn row(db: &Db, community: CommunityId, channel: Uuid, actor: &Keys) -> ChannelReadSummary {
    db.personal_read_sidebar_channels(
        community,
        &actor.public_key(),
        DEFAULT_RETENTION_SECONDS,
        &[channel],
    )
    .await
    .unwrap()
    .channels
    .remove(0)
}

/// The actor's frontier message for a thread, if the actor follows it. A
/// follower who has read nothing yet has no message.
async fn follow(
    pool: &PgPool,
    community: CommunityId,
    actor: &Keys,
    root: &nostr::Event,
) -> Option<Option<String>> {
    sqlx::query_scalar(
        "SELECT encode(through_message_id,'hex') FROM personal_read_frontiers
         WHERE community_id=$1 AND actor=$2 AND root_id=$3",
    )
    .bind(community.as_uuid())
    .bind(actor.public_key().to_bytes().as_slice())
    .bind(root.id.as_bytes().as_slice())
    .fetch_optional(pool)
    .await
    .unwrap()
}

/// (root, mentions) for each listed thread.
fn threads(row: &ChannelReadSummary) -> Vec<(String, u32)> {
    row.threads
        .iter()
        .map(|t| (t.root_id.clone(), t.mentions))
        .collect()
}

fn mark_thread(channel: Uuid, root: &nostr::Event, message: &str) -> ReadIntent {
    ReadIntent::MarkThrough {
        target: ReadTarget {
            channel_id: channel,
            root_id: Some(root.id.to_hex()),
        },
        message_id: message.to_owned(),
    }
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn replying_follows_the_thread_and_reads_through_the_reply() {
    let (db, pool, community, channel, actor, root) = fixture().await;
    let own = reply(&db, community, channel, &root, &actor, vec![]).await;
    assert_eq!(
        follow(&pool, community, &actor, &root).await,
        Some(Some(own.id.to_hex()))
    );
    assert!(row(&db, community, channel, &actor)
        .await
        .threads
        .is_empty());

    let other = reply(&db, community, channel, &root, &Keys::generate(), vec![]).await;
    let summary = row(&db, community, channel, &actor).await;
    // Unread, but not directed at the actor.
    assert_eq!(threads(&summary), [(root.id.to_hex(), 0)]);
    assert_eq!(summary.threads[0].latest_id, other.id.to_hex());
    assert_eq!(summary.threads[0].read_through_id, Some(own.id.to_hex()));
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_mention_follows_a_member_from_just_before_it() {
    let (db, pool, community, channel, _, root) = fixture().await;
    let bee = member(&db, &pool, community, channel).await;
    let outsider = Keys::generate();
    let earlier = reply(&db, community, channel, &root, &Keys::generate(), vec![]).await;
    let mut tags = mention(&bee);
    tags.extend(mention(&outsider));
    let mentioning = reply(&db, community, channel, &root, &Keys::generate(), tags).await;
    reply(&db, community, channel, &root, &Keys::generate(), vec![]).await;

    assert_eq!(follow(&pool, community, &bee, &root).await, Some(None));
    assert_eq!(follow(&pool, community, &outsider, &root).await, None);
    // The mention and what came after it are unread; not the reply before it.
    let summary = row(&db, community, channel, &bee).await;
    assert_eq!(threads(&summary), [(root.id.to_hex(), 1)]);
    assert_ne!(summary.threads[0].latest_id, earlier.id.to_hex());
    assert_ne!(summary.threads[0].latest_id, mentioning.id.to_hex());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_later_mention_keeps_the_followers_position() {
    let (db, pool, community, channel, actor, root) = fixture().await;
    let own = reply(&db, community, channel, &root, &actor, vec![]).await;
    reply(&db, community, channel, &root, &Keys::generate(), vec![]).await;
    reply(
        &db,
        community,
        channel,
        &root,
        &Keys::generate(),
        mention(&actor),
    )
    .await;
    assert_eq!(
        follow(&pool, community, &actor, &root).await,
        Some(Some(own.id.to_hex()))
    );
    let summary = row(&db, community, channel, &actor).await;
    assert_eq!(threads(&summary), [(root.id.to_hex(), 1)]);
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn the_root_author_follows_on_the_first_reply() {
    let (db, pool, community, channel, actor, _) = fixture().await;
    let root = post(&db, community, channel, &actor, vec![]).await;
    assert_eq!(follow(&pool, community, &actor, &root).await, None);

    reply(&db, community, channel, &root, &Keys::generate(), vec![]).await;
    assert_eq!(follow(&pool, community, &actor, &root).await, Some(None));
    let summary = row(&db, community, channel, &actor).await;
    assert_eq!(threads(&summary), [(root.id.to_hex(), 0)]);
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_top_level_mention_follows_the_thread_rooted_at_it() {
    let (db, pool, community, channel, actor, _) = fixture().await;
    let root = post(&db, community, channel, &Keys::generate(), mention(&actor)).await;
    assert_eq!(follow(&pool, community, &actor, &root).await, Some(None));
    reply(&db, community, channel, &root, &Keys::generate(), vec![]).await;

    let summary = row(&db, community, channel, &actor).await;
    // The fixture message and the mention are unread; one is a mention.
    assert_eq!((summary.unread, summary.mentions), (true, 1));
    // The reply is unread but does not mention the actor.
    assert_eq!(threads(&summary), [(root.id.to_hex(), 0)]);
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn dm_members_follow_every_thread() {
    let (db, pool, community, _, actor, _) = fixture().await;
    let (bee, wasp) = (Keys::generate(), Keys::generate());
    let keys = [&actor, &bee, &wasp].map(|k| k.public_key().to_bytes());
    let participants: Vec<&[u8]> = keys.iter().map(|k| k.as_slice()).collect();
    let dm = db
        .create_dm(community, &participants, &keys[1])
        .await
        .unwrap()
        .id;
    let root = post(&db, community, dm, &bee, vec![]).await;
    reply(&db, community, dm, &root, &wasp, vec![]).await;

    assert_eq!(follow(&pool, community, &actor, &root).await, Some(None));
    let summary = row(&db, community, dm, &actor).await;
    // Every DM message counts as a mention.
    assert_eq!((summary.unread, summary.mentions), (true, 1));
    assert_eq!(threads(&summary), [(root.id.to_hex(), 1)]);
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn an_unfollowed_thread_never_counts_and_marking_it_follows_nothing() {
    let (db, pool, community, channel, actor, root) = fixture().await;
    reply(&db, community, channel, &root, &Keys::generate(), vec![]).await;
    let broadcast = vec![Tag::parse(["broadcast", "1"]).unwrap()];
    let last = reply(&db, community, channel, &root, &Keys::generate(), broadcast).await;
    let summary = row(&db, community, channel, &actor).await;
    assert!(summary.threads.is_empty());
    // Only the broadcast reply is also on the timeline.
    assert_eq!((summary.unread, summary.mentions), (true, 0));
    assert_eq!(summary.latest_id, Some(last.id.to_hex()));

    let outcome = db
        .apply_personal_read_intent(
            community,
            &actor.public_key(),
            &mark_thread(channel, &root, &last.id.to_hex()),
        )
        .await
        .unwrap();
    assert_eq!(outcome, IntentOutcome::Applied);
    assert_eq!(follow(&pool, community, &actor, &root).await, None);
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn posting_reads_the_timeline_through_the_post() {
    let (db, _pool, community, channel, actor, _) = fixture().await;
    assert!(row(&db, community, channel, &actor).await.unread);
    let own = post(&db, community, channel, &actor, vec![]).await;
    let summary = row(&db, community, channel, &actor).await;
    assert!(!summary.unread);
    assert_eq!(summary.read_through_id, Some(own.id.to_hex()));
    assert_eq!(summary.latest_id, Some(own.id.to_hex()));
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn thread_counts_stop_at_the_cap() {
    let (db, _pool, community, channel, actor, root) = fixture().await;
    reply(&db, community, channel, &root, &actor, vec![]).await;
    let other = Keys::generate();
    for _ in 0..=MAX_UNREAD_COUNT {
        reply(&db, community, channel, &root, &other, mention(&actor)).await;
    }
    let summary = row(&db, community, channel, &actor).await;
    assert_eq!(
        threads(&summary),
        [(root.id.to_hex(), MAX_UNREAD_COUNT as u32)]
    );
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn ingest_does_not_start_an_account() {
    let (db, pool, community, channel, _, root) = fixture().await;
    let bee = Keys::generate();
    db.add_member(
        community,
        channel,
        &bee.public_key().to_bytes(),
        MemberRole::Member,
        None,
    )
    .await
    .unwrap();
    reply(
        &db,
        community,
        channel,
        &root,
        &Keys::generate(),
        mention(&bee),
    )
    .await;
    reply(&db, community, channel, &root, &bee, vec![]).await;

    let started: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
        "SELECT started_at FROM personal_read_accounts WHERE community_id=$1 AND actor=$2",
    )
    .bind(community.as_uuid())
    .bind(bee.public_key().to_bytes().as_slice())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(started, None);
    // Before its first intent, nothing counts.
    let summary = row(&db, community, channel, &bee).await;
    assert_eq!((summary.unread, summary.mentions), (false, 0));
    assert!(summary.threads.is_empty());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_nested_reply_follows_the_root_thread() {
    let (db, pool, community, channel, actor, _) = fixture().await;
    let root = post(&db, community, channel, &actor, vec![]).await;
    let parent = reply(&db, community, channel, &root, &Keys::generate(), vec![]).await;
    let bee = Keys::generate();
    let at = |e: &nostr::Event| {
        chrono::DateTime::from_timestamp(e.created_at.as_secs() as i64, 0).unwrap()
    };
    let nested = message(&bee, vec![]);
    db.insert_event_with_thread_metadata(
        community,
        &nested,
        Some(channel),
        Some(crate::event::ThreadMetadataParams {
            event_id: nested.id.as_bytes(),
            event_created_at: at(&nested),
            channel_id: channel,
            parent_event_id: Some(parent.id.as_bytes()),
            parent_event_created_at: Some(at(&parent)),
            root_event_id: Some(root.id.as_bytes()),
            root_event_created_at: Some(at(&root)),
            depth: 2,
            broadcast: false,
        }),
    )
    .await
    .unwrap();

    assert_eq!(
        follow(&pool, community, &bee, &root).await,
        Some(Some(nested.id.to_hex()))
    );
    assert_eq!(follow(&pool, community, &bee, &parent).await, None);
    let summary = row(&db, community, channel, &actor).await;
    assert_eq!(threads(&summary), [(root.id.to_hex(), 0)]);
    assert_eq!(summary.threads[0].latest_id, nested.id.to_hex());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_message_stored_without_thread_metadata_reads_the_timeline() {
    let (db, _pool, community, channel, actor, _) = fixture().await;
    let own = message(&actor, vec![]);
    db.insert_event_with_thread_metadata(community, &own, Some(channel), None)
        .await
        .unwrap();
    let summary = row(&db, community, channel, &actor).await;
    assert!(!summary.unread);
    assert_eq!(summary.read_through_id, Some(own.id.to_hex()));
}

fn follow_intent(channel: Uuid, root: Option<&nostr::Event>, follow: bool) -> ReadIntent {
    let target = ReadTarget {
        channel_id: channel,
        root_id: root.map(|r| r.id.to_hex()),
    };
    if follow {
        ReadIntent::Follow { target }
    } else {
        ReadIntent::Unfollow { target }
    }
}

async fn intent(
    db: &Db,
    community: CommunityId,
    actor: &Keys,
    intent: ReadIntent,
) -> IntentOutcome {
    db.apply_personal_read_intent(community, &actor.public_key(), &intent)
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn following_starts_now_and_needs_a_thread() {
    let (db, pool, community, channel, actor, root) = fixture().await;
    reply(&db, community, channel, &root, &Keys::generate(), vec![]).await;
    assert_eq!(
        intent(&db, community, &actor, follow_intent(channel, None, true)).await,
        IntentOutcome::Invalid
    );
    let missing = message(&Keys::generate(), vec![]);
    assert_eq!(
        intent(
            &db,
            community,
            &actor,
            follow_intent(channel, Some(&missing), true)
        )
        .await,
        IntentOutcome::Blocked
    );
    assert_eq!(
        intent(
            &db,
            community,
            &actor,
            follow_intent(channel, Some(&root), true)
        )
        .await,
        IntentOutcome::Applied
    );
    assert_eq!(follow(&pool, community, &actor, &root).await, Some(None));
    // The reply before the follow is read; the next one is not.
    assert!(row(&db, community, channel, &actor)
        .await
        .threads
        .is_empty());
    let next = reply(&db, community, channel, &root, &Keys::generate(), vec![]).await;
    let summary = row(&db, community, channel, &actor).await;
    assert_eq!(threads(&summary), [(root.id.to_hex(), 0)]);
    assert_eq!(summary.threads[0].latest_id, next.id.to_hex());
    // Following again keeps the position.
    intent(
        &db,
        community,
        &actor,
        follow_intent(channel, Some(&root), true),
    )
    .await;
    assert_eq!(
        threads(&row(&db, community, channel, &actor).await).len(),
        1
    );
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn an_unfollow_sticks_until_a_mention_or_own_post() {
    let (db, pool, community, channel, actor, _) = fixture().await;
    // The actor's own root: its author follows on every reply.
    let root = post(&db, community, channel, &actor, vec![]).await;
    reply(&db, community, channel, &root, &Keys::generate(), vec![]).await;
    assert_eq!(
        threads(&row(&db, community, channel, &actor).await).len(),
        1
    );
    assert_eq!(
        intent(
            &db,
            community,
            &actor,
            follow_intent(channel, Some(&root), false)
        )
        .await,
        IntentOutcome::Applied
    );
    reply(&db, community, channel, &root, &Keys::generate(), vec![]).await;
    assert!(row(&db, community, channel, &actor)
        .await
        .threads
        .is_empty());

    // A mention follows again, from just before it.
    let mentioning = reply(
        &db,
        community,
        channel,
        &root,
        &Keys::generate(),
        mention(&actor),
    )
    .await;
    let summary = row(&db, community, channel, &actor).await;
    assert_eq!(threads(&summary), [(root.id.to_hex(), 1)]);
    assert_eq!(summary.threads[0].latest_id, mentioning.id.to_hex());
    assert_eq!(summary.threads[0].read_through_id, None);

    // So does posting, which reads through the post.
    intent(
        &db,
        community,
        &actor,
        follow_intent(channel, Some(&root), false),
    )
    .await;
    let own = reply(&db, community, channel, &root, &actor, vec![]).await;
    assert_eq!(
        follow(&pool, community, &actor, &root).await,
        Some(Some(own.id.to_hex()))
    );
    reply(&db, community, channel, &root, &Keys::generate(), vec![]).await;
    assert_eq!(
        threads(&row(&db, community, channel, &actor).await).len(),
        1
    );
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_broadcast_reply_is_a_timeline_message_and_anchor() {
    let (db, _pool, community, channel, actor, root) = fixture().await;
    apply_mark(&db, community, channel, &actor, &root).await;
    let broadcast = vec![
        Tag::parse(["broadcast", "1"]).unwrap(),
        Tag::public_key(actor.public_key()),
    ];
    let last = reply(&db, community, channel, &root, &Keys::generate(), broadcast).await;
    let summary = row(&db, community, channel, &actor).await;
    assert_eq!((summary.unread, summary.mentions), (true, 1));
    assert_eq!(summary.latest_id, Some(last.id.to_hex()));
    assert_eq!(
        apply_mark(&db, community, channel, &actor, &last).await,
        IntentOutcome::Applied
    );
    assert!(!row(&db, community, channel, &actor).await.unread);

    // A broadcast by the reader reads the timeline through it, as a post does.
    post(&db, community, channel, &Keys::generate(), vec![]).await;
    let broadcast = vec![Tag::parse(["broadcast", "1"]).unwrap()];
    let own = reply(&db, community, channel, &root, &actor, broadcast).await;
    let summary = row(&db, community, channel, &actor).await;
    assert!(!summary.unread);
    assert_eq!(summary.read_through_id, Some(own.id.to_hex()));
}

async fn apply_mark(
    db: &Db,
    community: CommunityId,
    channel: Uuid,
    actor: &Keys,
    message: &nostr::Event,
) -> IntentOutcome {
    let mark = ReadIntent::MarkThrough {
        target: ReadTarget {
            channel_id: channel,
            root_id: None,
        },
        message_id: message.id.to_hex(),
    };
    intent(db, community, actor, mark).await
}

async fn threads_page(
    db: &Db,
    community: CommunityId,
    actor: &Keys,
    limit: usize,
    after: Option<&str>,
) -> ThreadsPage {
    let cursor = after.map(|c| ThreadsCursor::parse(c).unwrap());
    db.personal_read_threads(
        community,
        &actor.public_key(),
        DEFAULT_RETENTION_SECONDS,
        limit,
        cursor.as_ref(),
    )
    .await
    .unwrap()
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn the_threads_view_lists_follows_newest_arrival_first() {
    let (db, pool, community, channel, actor, _) = fixture().await;
    let mut roots = Vec::new();
    for _ in 0..3 {
        let root = post(&db, community, channel, &Keys::generate(), vec![]).await;
        reply(&db, community, channel, &root, &actor, vec![]).await;
        roots.push(root);
    }
    // The first thread has news, so it comes first; the rest are read.
    reply(
        &db,
        community,
        channel,
        &roots[0],
        &Keys::generate(),
        mention(&actor),
    )
    .await;
    // Neither an unfollowed thread nor one with no reply is listed.
    let quiet = post(&db, community, channel, &Keys::generate(), mention(&actor)).await;
    let left = post(&db, community, channel, &Keys::generate(), vec![]).await;
    reply(&db, community, channel, &left, &actor, vec![]).await;
    intent(
        &db,
        community,
        &actor,
        follow_intent(channel, Some(&left), false),
    )
    .await;
    assert_eq!(follow(&pool, community, &actor, &quiet).await, Some(None));

    let first = threads_page(&db, community, &actor, 2, None).await;
    let rows: Vec<_> = first
        .threads
        .iter()
        .map(|t| (t.thread.root_id.clone(), t.thread.unread, t.thread.mentions))
        .collect();
    assert_eq!(
        rows,
        [
            (roots[0].id.to_hex(), true, 1),
            (roots[2].id.to_hex(), false, 0)
        ]
    );
    assert!(first.threads.iter().all(|t| t.channel_id == channel));
    let rest = threads_page(&db, community, &actor, 2, first.next_cursor.as_deref()).await;
    let rest: Vec<_> = rest
        .threads
        .iter()
        .map(|t| t.thread.root_id.clone())
        .collect();
    assert_eq!(rest, [roots[1].id.to_hex()]);

    // Leaving the channel hides its threads.
    sqlx::query("UPDATE channel_members SET removed_at=now() WHERE community_id=$1 AND pubkey=$2")
        .bind(community.as_uuid())
        .bind(actor.public_key().to_bytes().as_slice())
        .execute(&pool)
        .await
        .unwrap();
    assert!(threads_page(&db, community, &actor, 2, None)
        .await
        .threads
        .is_empty());
}
