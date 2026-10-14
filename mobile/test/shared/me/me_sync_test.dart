import 'dart:async';
import 'dart:convert';
import 'package:flutter_test/flutter_test.dart';
import 'package:nostr/nostr.dart' as nostr;
import 'package:shared_preferences/shared_preferences.dart';
import 'package:buzz/shared/crypto/nip44.dart';
import 'package:buzz/shared/me/me_groups.dart';
import 'package:buzz/shared/me/me_sync.dart';
import 'package:buzz/shared/relay/relay.dart';

class FakeRelay extends RelaySessionNotifier {
  List<NostrEvent> events = [];
  int writes = 0;
  bool loseAcknowledgment = false;
  void Function()? beforeRead;
  @override
  Future<List<NostrEvent>> fetchHistory(
    NostrFilter filter, {
    Duration timeout = const Duration(seconds: 8),
  }) async {
    beforeRead?.call();
    return events;
  }

  @override
  Future<NostrEvent> publish(
    NostrEvent event, {
    Duration timeout = const Duration(seconds: 8),
  }) async {
    writes++;
    events = [event];
    if (loseAcknowledgment) throw TimeoutException('no acknowledgment');
    return event;
  }
}

void main() {
  const secret =
      '0000000000000000000000000000000000000000000000000000000000000001';
  final nsec = nostr.Nip19.encode(prefix: nostr.Nip19Prefix.nsec, data: secret);
  const origin = 'https://relay.test';
  final public = nostr.Keys(secret).public;
  NostrEvent signed({
    String community = origin,
    String signingKey = secret,
    int timestamp = 1,
    MeGroups groups = const MeGroups(),
  }) => NostrEvent.fromJson(
    nostr.Event.from(
      kind: 30078,
      content: nip44Encrypt(
        getConversationKey(secret, public),
        groups.encode(community),
      ),
      secretKey: signingKey,
      createdAt: timestamp,
      tags: [
        ['d', MeGroups.coordinate(community)],
        ['t', MeGroups.tag],
      ],
      verify: false,
    ).toMap(),
  );
  test('verifies signature, account and encrypted community envelope', () {
    final sync = MeSync(FakeRelay(), RelayConfig(baseUrl: origin, nsec: nsec));
    expect(sync.decode(signed()).channels, isEmpty);
    expect(
      () => sync.decode(signed(community: 'https://other.test')),
      throwsFormatException,
    );
    expect(
      () => sync.decode(
        signed(
          signingKey:
              '0000000000000000000000000000000000000000000000000000000000000002',
        ),
      ),
      throwsFormatException,
    );
    final tampered = signed().toJson()..['sig'] = '0' * 128;
    expect(() => sync.decode(NostrEvent.fromJson(tampered)), throwsA(anything));
  });
  test('equal timestamp selects the lowest event ID', () {
    final a = signed(timestamp: 2);
    final b = signed(timestamp: 2);
    expect(meEventIsNewer(a, b), a.id.compareTo(b.id) < 0);
    expect(meEventIsNewer(signed(timestamp: 3), a), isTrue);
  });
  test('scope disposal during preparation prevents publication', () async {
    final relay = FakeRelay();
    final sync = MeSync(relay, RelayConfig(baseUrl: origin, nsec: nsec));
    relay.beforeRead = () => sync.disposed = true;
    await expectLater(sync.update((v) => v), throwsStateError);
    expect(relay.writes, 0);
  });
  test('concurrent desktop change blocks stale replacement', () async {
    final relay = FakeRelay()..events = [signed()];
    final sync = MeSync(relay, RelayConfig(baseUrl: origin, nsec: nsec));
    var reads = 0;
    relay.beforeRead = () {
      if (++reads == 2) relay.events = [signed(timestamp: 2)];
    };
    await expectLater(sync.update((v) => v), throwsStateError);
    expect(relay.writes, 0);
  });
  test(
    'unconfirmed save survives service restart and never silently replaces it',
    () async {
      SharedPreferences.setMockInitialValues({});
      final prefs = await SharedPreferences.getInstance();
      final relay = FakeRelay()..loseAcknowledgment = true;
      final config = RelayConfig(baseUrl: origin, nsec: nsec);
      final first = MeSync(relay, config, prefs: prefs);
      await expectLater(
        first.update((v) => v),
        throwsA(isA<TimeoutException>()),
      );
      final pending = prefs.getString(first.pendingKey);
      expect(pending, isNotNull);
      expect(jsonDecode(pending!)['content'], isNot(contains('groups')));
      final second = MeSync(relay, config, prefs: prefs);
      await expectLater(second.update((v) => v), throwsStateError);
      expect(relay.writes, 1);
      expect(prefs.getString(first.pendingKey), isNull);
    },
  );
}
