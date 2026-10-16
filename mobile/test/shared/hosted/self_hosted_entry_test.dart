import 'dart:convert';

import 'package:buzz/shared/community/community.dart';
import 'package:buzz/shared/hosted/account_identity_store.dart';
import 'package:buzz/shared/hosted/device_identity_source.dart';
import 'package:buzz/shared/hosted/self_hosted_entry.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:nostr/nostr.dart' as nostr;

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  const relay = 'wss://community.example';
  // Public test vectors, never real user signing material.
  final key = nostr.Keys('1'.padLeft(64, '0'));
  final other = nostr.Keys('2'.padLeft(64, '0'));
  late _Storage storage;
  late List<Community> probes;
  late SelfHostedEntry entry;
  var current = true;
  var refuseProbe = false;

  SelfHostedEntry makeEntry() => SelfHostedEntry(
    storage: storage,
    identities: DeviceIdentitySource(storage: storage),
    accountStore: AccountIdentityStore(storage: storage),
    probe: (community) async {
      // The durable key must already exist before any network operation.
      expect(storage.data.values, contains(community.nsec));
      probes.add(community);
      if (refuseProbe) throw StateError('Connection refused');
    },
  );

  setUp(() {
    storage = _Storage();
    probes = [];
    current = true;
    refuseProbe = false;
    entry = makeEntry();
  });

  Future<Community> prepare({
    String address = relay,
    String? nsec,
    String? savedPubkey,
    bool create = false,
  }) => entry.prepare(
    relay: address,
    name: 'Community',
    nsec: nsec,
    savedPubkey: savedPubkey,
    create: create,
    isCurrent: () => current,
  );

  void expectUntouched() {
    expect(storage.writes, 0);
    expect(probes, isEmpty);
  }

  test(
    'reserved hosted domains refuse imported and generated identities',
    () async {
      for (final address in [
        'wss://team.communities.buzz.xyz',
        'wss://team.communities.public.builderlabstaging.xyz',
        'wss://BUILDERLAB.XYZ.',
      ]) {
        await expectLater(
          prepare(address: address, nsec: key.nsec),
          throwsStateError,
        );
        await expectLater(
          prepare(address: address, create: true),
          throwsStateError,
        );
      }
      expectUntouched();
      expect(storage.data, isEmpty);
    },
  );

  test('invalid addresses and invalid nsec never persist or probe', () async {
    for (final address in [
      'ws://community.example',
      'wss://user@community.example',
      'wss://community.example/path',
      'wss://community.example?token=value',
      'wss://community.example#fragment',
    ]) {
      await expectLater(
        prepare(address: address, nsec: key.nsec),
        throwsFormatException,
      );
    }
    for (final nsec in ['invalid', key.public, key.npub]) {
      await expectLater(
        prepare(nsec: nsec),
        throwsA(AccountIdentityFailure.invalidKey),
      );
    }
    expectUntouched();
    expect(storage.data, isEmpty);
  });

  test(
    'failed probe retains generated key across a new service and retry',
    () async {
      refuseProbe = true;
      await expectLater(prepare(create: true), throwsStateError);
      expect(storage.writes, 1);
      final persisted = Map<String, String>.of(storage.data);
      final first = probes.single;
      expect(first.pubkey, AccountIdentityStore.publicKeyForNsec(first.nsec!));
      refuseProbe = false;
      entry = makeEntry();
      final retried = await prepare(create: true);
      expect(retried.pubkey, first.pubkey);
      expect(retried.nsec, first.nsec);
      expect(storage.data, persisted);
      expect(storage.writes, 1);
      expect(probes, hasLength(2));
    },
  );

  test('existing local identity prevents generating a replacement', () async {
    storage.data['buzz_nsec'] = key.nsec;
    final before = Map<String, String>.of(storage.data);
    await expectLater(
      prepare(create: true),
      throwsA(AccountIdentityFailure.absent),
    );
    expectUntouched();
    expect(storage.data, before);
  });

  test(
    'explicit saved identity reuses key and preserves community records',
    () async {
      final raw = jsonEncode([
        {'name': 'Saved', 'nsec': key.nsec, 'pubkey': key.public},
      ]);
      storage.data['buzz_communities'] = raw;
      storage.data['buzz_nsec'] = other.nsec;
      final community = await prepare(savedPubkey: key.public);
      expect(community.nsec, key.nsec);
      expect(community.pubkey, key.public);
      expect(community.relayUrl, relay);
      expect(storage.data['buzz_communities'], raw);
      expect(storage.data['buzz_nsec'], other.nsec);
      expect(storage.writes, 1);
      expect(probes, [community]);
    },
  );

  test(
    'account-only saved identity blocks generation and supports explicit reuse',
    () async {
      final accountStore = AccountIdentityStore(storage: storage);
      await accountStore.create(key.nsec);
      final before = Map<String, String>.of(storage.data);
      final writesBefore = storage.writes;
      final choices = await entry.choices();
      expect(choices.map((choice) => choice.pubkey), [key.public]);
      await expectLater(
        prepare(create: true),
        throwsA(AccountIdentityFailure.absent),
      );
      expect(storage.writes, writesBefore);
      expect(probes, isEmpty);
      expect(storage.data, before);
      final community = await prepare(savedPubkey: key.public);
      expect(community.pubkey, key.public);
      expect(community.nsec, key.nsec);
      expect(
        storage.data[AccountIdentityStore.storageKey],
        before[AccountIdentityStore.storageKey],
      );
    },
  );

  test(
    'account and community choices deduplicate the same saved identity',
    () async {
      await AccountIdentityStore(storage: storage).create(key.nsec);
      storage.data['buzz_nsec'] = key.nsec;
      expect((await entry.choices()).map((choice) => choice.pubkey), [
        key.public,
      ]);
    },
  );

  test('unreadable account key never permits new generation', () async {
    storage.data[AccountIdentityStore.storageKey] = 'broken';
    await expectLater(
      prepare(create: true),
      throwsA(AccountIdentityFailure.unreadable),
    );
    expectUntouched();
  });

  test(
    'different import cannot replace retained key after a failed probe',
    () async {
      refuseProbe = true;
      await expectLater(prepare(nsec: key.nsec), throwsStateError);
      final before = Map<String, String>.of(storage.data);
      await expectLater(
        prepare(nsec: other.nsec),
        throwsA(AccountIdentityFailure.occupied),
      );
      expect(storage.data, before);
      expect(storage.writes, 1);
      expect(probes, hasLength(1));
    },
  );

  test(
    'missing explicit saved identity never falls back to generation',
    () async {
      await expectLater(
        prepare(savedPubkey: key.public, create: true),
        throwsA(AccountIdentityFailure.absent),
      );
      expectUntouched();
    },
  );

  test('cancellation before work performs no read, save or probe', () async {
    current = false;
    await expectLater(prepare(nsec: key.nsec), throwsStateError);
    expect(storage.reads, 0);
    expectUntouched();
  });

  test('cancellation during read prevents durable save and probe', () async {
    storage.onRead = () {
      current = false;
    };
    await expectLater(prepare(nsec: key.nsec), throwsStateError);
    expectUntouched();
    expect(storage.data, isEmpty);
  });

  test(
    'cancellation after durable save keeps key and prevents probe',
    () async {
      storage.onWrite = () {
        current = false;
      };
      await expectLater(prepare(nsec: key.nsec), throwsStateError);
      expect(storage.writes, 1);
      expect(storage.data.values, contains(key.nsec));
      expect(probes, isEmpty);
      current = true;
      storage.onWrite = null;
      expect((await prepare()).pubkey, key.public);
      expect(storage.writes, 1);
    },
  );

  test('unconfirmed durable write cannot reach the relay probe', () async {
    storage.dropWrites = true;
    await expectLater(
      prepare(nsec: key.nsec),
      throwsA(AccountIdentityFailure.unreadable),
    );
    expect(probes, isEmpty);
    expect(storage.data, isEmpty);
  });
}

class _Storage extends Fake implements FlutterSecureStorage {
  final data = <String, String>{};
  int writes = 0;
  int reads = 0;
  bool dropWrites = false;
  void Function()? onRead;
  void Function()? onWrite;

  @override
  Future<String?> read({
    required String key,
    AppleOptions? iOptions,
    AndroidOptions? aOptions,
    LinuxOptions? lOptions,
    WebOptions? webOptions,
    AppleOptions? mOptions,
    WindowsOptions? wOptions,
  }) async {
    reads++;
    onRead?.call();
    return data[key];
  }

  @override
  Future<void> write({
    required String key,
    required String? value,
    AppleOptions? iOptions,
    AndroidOptions? aOptions,
    LinuxOptions? lOptions,
    WebOptions? webOptions,
    AppleOptions? mOptions,
    WindowsOptions? wOptions,
  }) async {
    writes++;
    if (!dropWrites && value != null) data[key] = value;
    onWrite?.call();
  }
}
