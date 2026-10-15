import 'dart:convert';

import 'package:buzz/shared/hosted/account_identity_store.dart';
import 'package:buzz/shared/hosted/builderlab_account.dart';
import 'package:buzz/shared/hosted/device_identity_source.dart';
import 'package:buzz/shared/hosted/hosted_identity_service.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:nostr/nostr.dart' as nostr;

class InventoryAccount extends BuilderlabAccountClient {
  String? bound;
  List<String> backups = [];
  Map<String, dynamic> recovery = {};
  final calls = <String>[];
  @override
  BuilderlabAccount get account =>
      BuilderlabAccount(subject: 'fixture', expiresAt: DateTime.utc(2100));
  @override
  Future<BuilderlabAccount?> restore() async => account;
  @override
  Future<Map<String, dynamic>> currentIdentity() async => {
    if (bound != null) 'identity': {'pubkey_hex': bound},
  };
  @override
  Future<Map<String, dynamic>> post(
    String path,
    Map<String, dynamic> body,
  ) async {
    calls.add(path);
    if (path.endsWith('/status')) {
      return {'current_nostr_pubkey': bound, 'backup_nostr_pubkeys': backups};
    }
    if (path.endsWith('/recovery')) return recovery;
    throw AccountFailure.denied;
  }
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  final first = nostr.Keys('1'.padLeft(64, '0'));
  final second = nostr.Keys('2'.padLeft(64, '0'));
  const secure = FlutterSecureStorage();
  late InventoryAccount account;
  late AccountIdentityStore store;
  late HostedIdentityService service;
  Map<String, dynamic> record(nostr.Keys key, String name) => {
    'nsec': key.nsec,
    'pubkey': key.public,
    'name': name,
  };
  setUp(() {
    FlutterSecureStorage.setMockInitialValues({});
    account = InventoryAccount();
    store = AccountIdentityStore();
    service = HostedIdentityService(
      account: account,
      store: store,
      deviceIdentities: DeviceIdentitySource(),
    );
  });
  tearDown(() => account.dispose());

  test(
    'single legacy key is reused without migrating or deleting any device record',
    () async {
      final raw = jsonEncode([record(first, 'Old community')]);
      await secure.write(key: 'buzz_workspaces', value: raw);
      final result = await service.resolve();
      expect(result.pubkey, first.public);
      expect(result.state, HostedIdentityState.needsBinding);
      expect(await secure.read(key: 'buzz_workspaces'), raw);
      expect(await secure.read(key: 'buzz_communities'), isNull);
      expect(account.calls, ['/v1/buzz/nsec-backups/status']);
    },
  );

  test(
    'multiple current and hidden legacy keys require choice; no writes before choice',
    () async {
      await secure.write(
        key: 'buzz_communities',
        value: jsonEncode([record(first, 'Current')]),
      );
      await secure.write(key: 'buzz_nsec', value: second.nsec);
      final before = await secure.readAll();
      final pending = await service.resolve();
      expect(pending.state, HostedIdentityState.needsSelection);
      expect(pending.choices.map((item) => item.pubkey), [
        first.public,
        second.public,
      ]);
      expect(await secure.readAll(), before);
      final selected = await service.resolve(selectedPubkey: second.public);
      expect(selected.pubkey, second.public);
      for (final entry in before.entries) {
        expect(await secure.read(key: entry.key), entry.value);
      }
      expect(account.calls.every((path) => path.endsWith('/status')), isTrue);
      expect(
        (await service.resolve(selectedPubkey: first.public)).pubkey,
        second.public,
      );
    },
  );

  test(
    'duplicate identities auto-resolve and truly empty inventory generates once',
    () async {
      final firstResult = await service.resolve();
      expect(firstResult.state, HostedIdentityState.needsBinding);
      expect((await service.resolve()).pubkey, firstResult.pubkey);
      FlutterSecureStorage.setMockInitialValues({
        'buzz_nsec': first.nsec,
        'buzz_communities': jsonEncode([record(first, 'Same identity')]),
      });
      expect((await service.resolve()).pubkey, first.public);
    },
  );

  test(
    'corrupt, incomplete and mismatched legacy records never authorize generation',
    () async {
      for (final data in [
        {'buzz_workspaces': '{broken'},
        {'buzz_nsec': ''},
        {'buzz_pubkey': first.public},
        {
          'buzz_communities': jsonEncode([
            {'nsec': first.nsec, 'pubkey': second.public},
          ]),
        },
      ]) {
        FlutterSecureStorage.setMockInitialValues(data);
        await expectLater(
          service.resolve(),
          throwsA(AccountIdentityFailure.unreadable),
        );
        expect(await secure.readAll(), data);
      }
    },
  );

  test(
    'cloud backup differing from device requires consent and preserves old key',
    () async {
      await secure.write(key: 'buzz_nsec', value: first.nsec);
      account.bound = second.public;
      account.backups = [second.public];
      account.recovery = {
        'backups': [
          {
            'backup_id': 'fixture-backup',
            'nostr_pubkey': second.public,
            'secret_key': base64UrlEncode([
              ...List.filled(31, 0),
              2,
            ]).replaceAll('=', ''),
          },
        ],
        'failed_backup_ids': [],
      };
      expect(
        (await service.resolve()).state,
        HostedIdentityState.needsSelection,
      );
      expect(await store.read(), isNull);
      expect(
        (await service.resolve(selectedPubkey: first.public)).state,
        HostedIdentityState.needsSelection,
      );
      expect(await store.read(), isNull);
      expect(
        (await service.resolve(selectedPubkey: second.public)).state,
        HostedIdentityState.ready,
      );
      expect(await secure.read(key: 'buzz_nsec'), first.nsec);
      expect(
        account.calls.every(
          (path) => path.endsWith('/status') || path.endsWith('/recovery'),
        ),
        isTrue,
      );
    },
  );

  test(
    'existing account slot cannot overwrite or bind over a different cloud backup',
    () async {
      await store.create(first.nsec);
      final before = await secure.readAll();
      account.backups = [second.public];
      expect((await service.resolve()).state, HostedIdentityState.mismatch);
      expect(await secure.readAll(), before);
    },
  );

  test('cancelled resolution cannot adopt or generate', () async {
    await secure.write(key: 'buzz_nsec', value: first.nsec);
    await expectLater(
      service.resolve(isCurrent: () => false),
      throwsA(AccountFailure.unavailable),
    );
    expect(await store.read(), isNull);
  });
}
