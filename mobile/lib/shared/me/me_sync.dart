import 'dart:async';
import 'dart:convert';

import 'package:flutter/widgets.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';
import 'package:nostr/nostr.dart' as nostr;
import 'package:shared_preferences/shared_preferences.dart';
import '../theme/theme_provider.dart';

import '../crypto/nip44.dart';
import '../relay/relay.dart';
import 'me_groups.dart';

/// A live, account/community-scoped reader for desktop's encrypted Me record.
final meGroupsProvider = StreamProvider<MeGroups>((ref) {
  final service = ref.watch(meSyncProvider);
  final controller = StreamController<MeGroups>();
  var disposed = false;
  void Function()? unsubscribe;
  NostrEvent? latest;
  void accept(NostrEvent event) {
    if (disposed ||
        (latest != null &&
            event.id != latest!.id &&
            !meEventIsNewer(event, latest!))) {
      return;
    }
    try {
      final value = service.decode(event);
      latest = event;
      controller.add(value);
    } catch (error, stack) {
      controller.addError(error, stack);
    }
  }

  Future<void> refresh() async {
    try {
      final event = await service.head();
      if (disposed) return;
      if (event != null) {
        accept(event);
      } else if (latest == null) {
        controller.add(const MeGroups());
      }
    } catch (error, stack) {
      if (!disposed) controller.addError(error, stack);
    }
  }

  Future<void> start() async {
    try {
      service.check();
      unsubscribe = await service.session.subscribe(
        service.filter,
        accept,
        onClosed: (_) {
          if (!disposed) {
            controller.addError(
              StateError('Couldn’t sync Me. Reconnect and try again.'),
            );
          }
        },
      );
      if (disposed) {
        unsubscribe?.call();
        return;
      }
      await refresh();
    } catch (error, stack) {
      if (!disposed) controller.addError(error, stack);
    }
  }

  ref.listen(appLifecycleProvider, (_, next) {
    if (next == AppLifecycleState.resumed) unawaited(refresh());
  });
  ref.onDispose(() {
    disposed = true;
    unsubscribe?.call();
    unawaited(controller.close());
  });
  unawaited(start());
  return controller.stream;
});

/// NIP-01 replacement ordering: smallest ID wins an equal timestamp.
bool meEventIsNewer(NostrEvent a, NostrEvent b) =>
    a.createdAt > b.createdAt ||
    (a.createdAt == b.createdAt && a.id.compareTo(b.id) < 0);

final meSyncProvider = Provider<MeSync>((ref) {
  final config = ref.watch(relayConfigProvider);
  final status = ref.watch(relaySessionProvider).status;
  final service = MeSync(
    ref.read(relaySessionProvider.notifier),
    config,
    connected: status == SessionStatus.connected,
    prefs: ref.read(savedPrefsProvider),
  );
  ref.onDispose(() => service.disposed = true);
  return service;
});

/// Narrow private-recipe transport; never writes Messages organization.
class MeSync {
  final RelaySessionNotifier session;
  final RelayConfig config;
  late final String community = Uri.parse(config.baseUrl).origin;
  late final String privateHex = nostr.Nip19.decode(payload: config.nsec!).data;
  late final String pubkey = nostr.Keys(privateHex).public;
  late final key = getConversationKey(privateHex, pubkey);
  final bool connected;
  final SharedPreferences? prefs;
  String get pendingKey => 'me-pending:$community:$pubkey';
  bool disposed = false;
  bool _saving = false;

  MeSync(this.session, this.config, {this.connected = true, this.prefs});

  NostrFilter get filter => NostrFilter(
    kinds: const [30078],
    authors: [pubkey],
    tags: {
      '#d': [MeGroups.coordinate(community)],
    },
    limit: 1,
  );

  void check() {
    if (!connected || config.nsec == null) {
      throw StateError('Connect to your community to load Me.');
    }
    if (disposed) throw StateError('Your community changed. Open Me again.');
  }

  MeGroups decode(NostrEvent event) {
    if (event.kind != 30078 ||
        event.pubkey != pubkey ||
        event.tags.where((t) => t.isNotEmpty && t[0] == 'd').length != 1 ||
        !event.tags.any(
          (t) =>
              t.length == 2 &&
              t[0] == 'd' &&
              t[1] == MeGroups.coordinate(community),
        ) ||
        !event.tags.any(
          (t) => t.length == 2 && t[0] == 't' && t[1] == MeGroups.tag,
        )) {
      throw const FormatException('Invalid Me record');
    }
    nostr.Event.fromJson(jsonEncode(event.toJson()));
    return MeGroups.decode(nip44Decrypt(key, event.content), community);
  }

  Future<NostrEvent?> head() async {
    check();
    final events = await session.fetchHistory(filter);
    check();
    if (events.isEmpty) return null;
    final event = events.reduce((a, b) => meEventIsNewer(a, b) ? a : b);
    decode(event);
    return event;
  }

  /// Refresh before applying an intent, then confirm the selected signed event.
  Future<void> update(MeGroups Function(MeGroups) change) async {
    check();
    if (_saving) throw StateError('A Me change is still being saved.');
    _saving = true;
    try {
      final pending = prefs?.getString(pendingKey);
      if (pending != null) {
        final event = NostrEvent.fromJson(
          jsonDecode(pending) as Map<String, dynamic>,
        );
        decode(event);
        if ((await head())?.id != event.id) {
          check();
          await session.publish(event);
          if ((await head())?.id != event.id) {
            throw StateError(
              'A previous Me save is still unconfirmed. Refresh before making another change.',
            );
          }
        }
        await prefs?.remove(pendingKey);
        throw StateError(
          'Your previous Me save is confirmed. Review your projects before making another change.',
        );
      }
      final before = await head();
      final value = change(before == null ? const MeGroups() : decode(before));
      final content = nip44Encrypt(key, value.encode(community));
      final current = await head();
      if (before?.id != current?.id) {
        throw StateError(
          'Me changed on another device. Refresh and try again.',
        );
      }
      if (current != null &&
          current.createdAt >= DateTime.now().millisecondsSinceEpoch ~/ 1000) {
        throw StateError('Please wait a second before saving again.');
      }
      check();
      final signed = NostrEvent.fromJson(
        nostr.Event.from(
          kind: 30078,
          content: content,
          secretKey: privateHex,
          verify: false,
          tags: [
            ['d', MeGroups.coordinate(community)],
            ['t', MeGroups.tag],
          ],
        ).toMap(),
      );
      if (prefs != null &&
          !await prefs!.setString(pendingKey, jsonEncode(signed.toJson()))) {
        throw StateError('Couldn’t save this change on your phone. Try again.');
      }
      check();
      await session.publish(signed);
      check();
      if ((await head())?.id != signed.id) {
        throw StateError(
          'Another Me change is selected. Refresh to see the latest projects.',
        );
      }
      await prefs?.remove(pendingKey);
    } finally {
      _saving = false;
    }
  }
}
