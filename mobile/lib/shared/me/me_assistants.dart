import 'dart:convert';

import 'package:nostr/nostr.dart' as nostr;
import 'package:hooks_riverpod/hooks_riverpod.dart';

import '../crypto/nip_oa.dart';
import '../mentions/agent_identity_provider.dart';
import '../relay/relay.dart';
import 'me_sync.dart';

/// Owned assistants from verified profiles, including agents without kind 10100.
/// Agents inherit their owner's community access and need not be listed in the
/// community's human membership snapshot.
final meAssistantsProvider = FutureProvider<List<AgentDirectoryEntry>>((
  ref,
) async {
  final sync = ref.watch(meSyncProvider);
  final session = ref.watch(relaySessionProvider.notifier);
  return _loadAssistants(sync, session);
});

/// Freshness-controlled ownership check for one personal-chat recipient.
final meAssistantOwnershipProvider = FutureProvider.family<bool, String>((
  ref,
  pubkey,
) {
  return isOwnedMeAssistant(ref.watch(meSyncProvider), pubkey);
});

/// Revalidates only the chosen assistant before creating a chat.
Future<bool> isOwnedMeAssistant(MeSync sync, String pubkey) async =>
    (await _loadAssistants(
      sync,
      sync.session,
      pubkey: pubkey,
    )).any((agent) => agent.pubkey == pubkey);

Future<List<AgentDirectoryEntry>> _loadAssistants(
  MeSync sync,
  RelaySessionNotifier session, {
  String? pubkey,
}) async {
  final profiles = <String, NostrEvent>{};
  final seen = <String>{};
  const pageSize = 100;
  for (var page = 1; ; page++) {
    sync.check();
    final events = await session.queryRelay([
      NostrFilter(
        kinds: const [0],
        authors: pubkey == null ? null : [pubkey],
        limit: pageSize,
        extensions: {'page': page},
      ),
    ]);
    sync.check();
    var added = false;
    for (final event in events) {
      if (event.kind != 0 || (pubkey != null && event.pubkey != pubkey)) {
        continue;
      }
      try {
        nostr.Event.fromJson(jsonEncode(event.toJson()));
      } catch (_) {
        continue;
      }
      if (!seen.add(event.id)) continue;
      added = true;
      final previous = profiles[event.pubkey];
      if (previous == null ||
          event.createdAt > previous.createdAt ||
          (event.createdAt == previous.createdAt &&
              event.id.compareTo(previous.id) < 0)) {
        profiles[event.pubkey] = event;
      }
    }
    if (events.length < pageSize) break;
    if (!added) {
      throw StateError('Could not finish loading assistants. Try again.');
    }
  }
  return [
    for (final event in profiles.values)
      if (verifiedOaOwnerPubkey(event.tags, event.pubkey) == sync.pubkey)
        AgentDirectoryEntry.fromEvent(event),
  ];
}
