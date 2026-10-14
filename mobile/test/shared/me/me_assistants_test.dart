import 'dart:convert';
import 'dart:typed_data';
import 'package:buzz/shared/me/me_assistants.dart';
import 'package:buzz/shared/me/me_sync.dart';
import 'package:buzz/shared/relay/relay.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';
import 'package:nostr/nostr.dart' as nostr;
import 'package:pointycastle/digests/sha256.dart';

class _Relay extends RelaySessionNotifier {
  final List<List<NostrEvent>> pages;
  final requestedPages = <int>[];
  final requestedAuthors = <List<String>?>[];
  _Relay(this.pages);
  @override
  SessionState build() => const SessionState(status: SessionStatus.connected);
  @override
  Future<List<NostrEvent>> queryRelay(
    List<NostrFilter> filters, {
    Duration timeout = const Duration(seconds: 8),
  }) async {
    expect(filters.single.kinds, [0]);
    requestedAuthors.add(filters.single.authors);
    final page = filters.single.toJson()['page'] as int;
    requestedPages.add(page);
    return pages[page - 1];
  }
}

void main() {
  final owner = nostr.Keys.generate();
  final other = nostr.Keys.generate();
  final agent = nostr.Keys.generate();
  NostrEvent profile(nostr.Keys signer, {nostr.Keys? ownedBy, int time = 1}) {
    final digest = SHA256Digest()
        .process(
          Uint8List.fromList(utf8.encode('nostr:agent-auth:${signer.public}:')),
        )
        .map((v) => v.toRadixString(16).padLeft(2, '0'))
        .join();
    return NostrEvent.fromJson(
      nostr.Event.from(
        kind: 0,
        content: jsonEncode({'display_name': 'My assistant'}),
        secretKey: signer.secret,
        createdAt: time,
        tags: [
          if (ownedBy != null)
            [
              'auth',
              ownedBy.public,
              '',
              nostr.Schnorr.sign(secretKey: ownedBy.secret, message: digest),
            ],
        ],
      ).toMap(),
    );
  }

  ProviderContainer setup(_Relay relay) {
    final sync = MeSync(
      relay,
      RelayConfig(
        baseUrl: 'https://relay.test',
        nsec: nostr.Nip19.encode(
          prefix: nostr.Nip19Prefix.nsec,
          data: owner.secret,
        ),
      ),
    );
    final container = ProviderContainer(
      overrides: [
        relaySessionProvider.overrideWith(() => relay),
        meSyncProvider.overrideWithValue(sync),
      ],
    );
    addTearDown(container.dispose);
    return container;
  }

  test('finds owned kind 0 agents without legacy directory entries', () async {
    final valid = profile(agent, ownedBy: owner);
    final forged = valid.toJson()..['pubkey'] = other.public;
    final container = setup(
      _Relay([
        [
          profile(owner),
          profile(nostr.Keys.generate(), ownedBy: other),
          NostrEvent.fromJson(forged),
          valid,
        ],
      ]),
    );
    final choices = await container.read(meAssistantsProvider.future);
    expect(choices.map((a) => a.pubkey), [agent.public]);
    expect(choices.single.displayName, 'My assistant');
  });
  test('latest profile removes obsolete ownership', () async {
    final container = setup(
      _Relay([
        [profile(agent, ownedBy: owner), profile(agent, time: 2)],
      ]),
    );
    expect(await container.read(meAssistantsProvider.future), isEmpty);
  });
  test('reads beyond the first directory page', () async {
    final relay = _Relay([
      [for (var i = 0; i < 100; i++) profile(owner, time: i + 1)],
      [profile(agent, ownedBy: owner)],
    ]);
    expect(
      (await setup(relay).read(meAssistantsProvider.future)).single.pubkey,
      agent.public,
    );
    expect(relay.requestedPages, [1, 2]);
  });
  test('creation revalidates only the chosen assistant', () async {
    final relay = _Relay([
      [profile(agent, ownedBy: owner)],
    ]);
    final container = setup(relay);
    expect(
      await isOwnedMeAssistant(container.read(meSyncProvider), agent.public),
      isTrue,
    );
    expect(relay.requestedAuthors, [
      [agent.public],
    ]);
  });
  test(
    'creation rejects revoked ownership and unrelated returned profiles',
    () async {
      final relay = _Relay([
        [
          profile(agent, ownedBy: owner),
          profile(agent, time: 2),
          profile(other, ownedBy: owner),
        ],
      ]);
      final container = setup(relay);
      expect(
        await isOwnedMeAssistant(container.read(meSyncProvider), agent.public),
        isFalse,
      );
    },
  );
}
