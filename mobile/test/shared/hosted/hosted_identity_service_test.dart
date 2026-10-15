import 'dart:convert';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:nostr/nostr.dart' as nostr;
import 'package:buzz/shared/hosted/account_identity_store.dart';
import 'package:buzz/shared/hosted/builderlab_account.dart';
import 'package:buzz/shared/hosted/hosted_identity_service.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  const hex =
      '0000000000000000000000000000000000000000000000000000000000000001';
  final key = nostr.Keys(hex);
  final other = nostr.Keys(
    '0000000000000000000000000000000000000000000000000000000000000002',
  );
  late List<String> calls;
  late Map<String, dynamic> binding;
  late Map<String, dynamic> recovery;
  late Map<String, dynamic> status;
  var statusCode = 200;
  setUp(() {
    FlutterSecureStorage.setMockInitialValues({});
    calls = [];
    statusCode = 200;
    status = {
      'current_nostr_pubkey': key.public,
      'backup_nostr_pubkeys': [key.public],
    };
    binding = {
      'identity': {'pubkey_hex': key.public},
    };
    recovery = {
      'backups': [
        {
          'backup_id': 'one',
          'nostr_pubkey': key.public,
          'secret_key': base64Url
              .encode([...List.filled(31, 0), 1])
              .replaceAll('=', ''),
        },
      ],
      'failed_backup_ids': [],
    };
  });
  Future<BuilderlabAccountClient> client() async {
    final expiry = DateTime.now()
        .toUtc()
        .add(const Duration(hours: 1))
        .toIso8601String();
    final client = BuilderlabAccountClient(
      httpClient: MockClient((request) async {
        calls.add(request.url.path);
        final path = request.url.path;
        if (path.endsWith('/exchange')) {
          return http.Response(
            jsonEncode({'session_credential': 'token', 'expires_at': expiry}),
            200,
          );
        }
        if (path.endsWith('/me')) {
          return http.Response(
            jsonEncode({'subject': 'a', 'expires_at': expiry}),
            200,
          );
        }
        if (path.endsWith('/current')) {
          return http.Response(jsonEncode(binding), 200);
        }
        if (path.endsWith('/status')) {
          return http.Response(jsonEncode(status), statusCode);
        }
        if (path.endsWith('/recovery')) {
          return http.Response(jsonEncode(recovery), 200);
        }
        return http.Response('{}', 404);
      }),
    );
    await client.exchange(code: 'code', verifier: 'a' * 43);
    return client;
  }

  test(
    'local identity skips recovery and mismatch never replaces it',
    () async {
      final store = AccountIdentityStore();
      await store.create(key.nsec);
      final service = HostedIdentityService(
        account: await client(),
        store: store,
      );
      expect((await service.resolve()).state, HostedIdentityState.needsBackup);
      binding = {
        'identity': {'pubkey_hex': other.public},
      };
      expect((await service.resolve()).state, HostedIdentityState.mismatch);
      expect(calls.any((p) => p.endsWith('/recovery')), isFalse);
      expect((await store.read())!.pubkey, key.public);
    },
  );
  test(
    'recovery persists scoped receipt then restart skips duplicate recovery',
    () async {
      final account = await client();
      expect(
        (await HostedIdentityService(
          account: account,
          store: AccountIdentityStore(),
        ).resolve()).state,
        HostedIdentityState.ready,
      );
      expect(
        (await HostedIdentityService(
          account: account,
          store: AccountIdentityStore(),
        ).resolve()).state,
        HostedIdentityState.ready,
      );
      expect(calls.where((p) => p.endsWith('/recovery')).length, 1);
    },
  );
  test('partial and inconsistent recovery never persist', () async {
    final service = HostedIdentityService(
      account: await client(),
      store: AccountIdentityStore(),
    );
    recovery['failed_backup_ids'] = ['failed'];
    expect(
      (await service.resolve()).state,
      HostedIdentityState.recoveryBlocked,
    );
    recovery['failed_backup_ids'] = [];
    (recovery['backups'] as List).first['nostr_pubkey'] = other.public;
    await expectLater(
      service.resolve(),
      throwsA(AccountFailure.invalidResponse),
    );
    expect(await AccountIdentityStore().read(), isNull);
  });
  test('distinct recovered keys block even if one matches binding', () async {
    (recovery['backups'] as List).add({
      'backup_id': 'two',
      'nostr_pubkey': other.public,
      'secret_key': base64Url
          .encode([...List.filled(31, 0), 2])
          .replaceAll('=', ''),
    });
    final service = HostedIdentityService(
      account: await client(),
      store: AccountIdentityStore(),
    );
    expect(
      (await service.resolve()).state,
      HostedIdentityState.recoveryBlocked,
    );
    expect(await AccountIdentityStore().read(), isNull);
  });

  test('empty recovery and unbound does not generate', () async {
    binding = {};
    status = {'backup_nostr_pubkeys': []};
    recovery = {'backups': [], 'failed_backup_ids': []};
    final service = HostedIdentityService(
      account: await client(),
      store: AccountIdentityStore(),
    );
    expect(
      (await service.resolve()).state,
      HostedIdentityState.needsAuthoritativeAbsence,
    );
    expect(await AccountIdentityStore().read(), isNull);
  });
  test(
    'authoritative empty status generates only after legacy gate and reuses on retry',
    () async {
      binding = {};
      status = {'backup_nostr_pubkeys': []};
      final store = AccountIdentityStore();
      final service = HostedIdentityService(
        account: await client(),
        store: store,
        canGenerateAfterLegacyReconciliation: () async => true,
      );
      final first = await service.resolve();
      expect(first.state, HostedIdentityState.needsBinding);
      expect((await service.resolve()).pubkey, first.pubkey);
      expect(calls.where((p) => p.endsWith('/status')).length, 1);
      expect(calls.any((p) => p.endsWith('/recovery')), isFalse);
    },
  );

  test(
    'missing status list, failed status, legacy conflict and cancellation cannot generate',
    () async {
      binding = {};
      final store = AccountIdentityStore();
      var current = true;
      final service = HostedIdentityService(
        account: await client(),
        store: store,
        canGenerateAfterLegacyReconciliation: () async {
          current = false;
          return true;
        },
      );
      for (final code in [403, 404, 500]) {
        statusCode = code;
        await expectLater(service.resolve(), throwsA(isA<AccountFailure>()));
        expect(await store.read(), isNull);
      }
      statusCode = 200;
      status = {};
      await expectLater(
        service.resolve(),
        throwsA(AccountFailure.invalidResponse),
      );
      status = {'backup_nostr_pubkeys': []};
      await expectLater(
        service.resolve(isCurrent: () => current),
        throwsA(AccountFailure.unavailable),
      );
      expect(await store.read(), isNull);
      final blocked = HostedIdentityService(
        account: await client(),
        store: store,
        canGenerateAfterLegacyReconciliation: () async => false,
      );
      expect(
        (await blocked.resolve()).state,
        HostedIdentityState.needsAuthoritativeAbsence,
      );
      expect(await store.read(), isNull);
    },
  );
}
