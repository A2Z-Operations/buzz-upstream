import 'dart:async';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:nostr/nostr.dart' as nostr;
import 'package:buzz/shared/hosted/account_identity_store.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  final key = nostr.Keys(
    '0000000000000000000000000000000000000000000000000000000000000001',
  );
  setUp(() => FlutterSecureStorage.setMockInitialValues({}));

  test('recovery atomically retains scoped receipt across restart', () async {
    final receipt = BackupReceipt(
      backend: 'https://app.builderlab.xyz',
      subject: 'account-a',
      pubkey: key.public,
      backupId: 'backup-1',
    );
    final store = AccountIdentityStore();
    expect(await store.read(), isNull);
    await store.create(key.nsec, recoveredBackup: receipt);
    final restored = (await AccountIdentityStore().read())!;
    expect(
      restored.hasBackupFor(backend: receipt.backend, subject: receipt.subject),
      isTrue,
    );
    expect(
      restored.hasBackupFor(backend: receipt.backend, subject: 'account-b'),
      isFalse,
    );
    expect(await store.withKeys((keys) async => keys.public), key.public);
    expect((await store.create(key.nsec)).pubkey, key.public);
  });

  test(
    'unreadable storage never allows creation or touches legacy keys',
    () async {
      FlutterSecureStorage.setMockInitialValues({
        AccountIdentityStore.storageKey: 'broken',
        'buzz_communities': 'legacy',
      });
      final store = AccountIdentityStore();
      await expectLater(
        store.read(),
        throwsA(AccountIdentityFailure.unreadable),
      );
      await expectLater(
        store.create(key.nsec),
        throwsA(AccountIdentityFailure.unreadable),
      );
      expect(
        await const FlutterSecureStorage().read(key: 'buzz_communities'),
        'legacy',
      );
    },
  );

  test('invalid candidate and wrong-key receipt never write', () async {
    final store = AccountIdentityStore();
    await expectLater(
      store.create('not-a-key'),
      throwsA(AccountIdentityFailure.invalidKey),
    );
    final other = nostr.Keys(
      '0000000000000000000000000000000000000000000000000000000000000002',
    );
    await expectLater(
      store.create(
        key.nsec,
        recoveredBackup: BackupReceipt(
          backend: 'https://app.builderlab.xyz',
          subject: 'a',
          pubkey: other.public,
          backupId: 'b',
        ),
      ),
      throwsA(AccountIdentityFailure.mismatch),
    );
    expect(await store.read(), isNull);
    await store.create(key.nsec);
    await expectLater(
      store.confirmBackup(
        BackupReceipt(
          backend: 'https://app.builderlab.xyz',
          subject: 'a',
          pubkey: other.public,
          backupId: 'b',
        ),
      ),
      throwsA(AccountIdentityFailure.mismatch),
    );
    expect((await store.read())!.backup, isNull);
  });

  test(
    'uncertain generation write retries without authorizing or generating again',
    () async {
      final storage = UncertainStorage();
      final store = AccountIdentityStore(storage: storage);
      var authorizations = 0;
      Future<bool> authorize() async {
        authorizations++;
        return true;
      }

      await expectLater(
        store.generateIfAbsent(authorize: authorize),
        throwsA(AccountIdentityFailure.unreadable),
      );
      final persisted = await store.read();
      final retried = await store.generateIfAbsent(authorize: authorize);
      expect(retried!.pubkey, persisted!.pubkey);
      expect(authorizations, 1);
      expect(storage.writes, 1);
    },
  );

  test(
    'uncertain completed write retries exact key and keeps receipt',
    () async {
      final storage = UncertainStorage();
      final store = AccountIdentityStore(storage: storage);
      final receipt = BackupReceipt(
        backend: 'https://app.builderlab.xyz',
        subject: 'a',
        pubkey: key.public,
        backupId: 'one',
      );
      await expectLater(
        store.create(key.nsec, recoveredBackup: receipt),
        throwsA(AccountIdentityFailure.unreadable),
      );
      final retried = await store.create(key.nsec, recoveredBackup: receipt);
      expect(retried.backup?.backupId, 'one');
      final other = nostr.Keys(
        '0000000000000000000000000000000000000000000000000000000000000002',
      );
      await expectLater(
        store.create(other.nsec),
        throwsA(AccountIdentityFailure.occupied),
      );
      await expectLater(
        store.create(
          key.nsec,
          recoveredBackup: BackupReceipt(
            backend: receipt.backend,
            subject: 'b',
            pubkey: key.public,
            backupId: 'two',
          ),
        ),
        throwsA(AccountIdentityFailure.mismatch),
      );
      expect(storage.writes, 1);
    },
  );

  test(
    'late durable write blocks second instance create until settled',
    () async {
      final storage = DelayedStorage();
      final first = AccountIdentityStore(storage: storage).create(key.nsec);
      await storage.started.future;
      final second = AccountIdentityStore(storage: storage).create(key.nsec);
      final retried = second;
      storage.release.complete();
      await first;
      await retried;
      expect(storage.writes, 1);
    },
  );
}

class DelayedStorage extends FlutterSecureStorage {
  final started = Completer<void>();
  final release = Completer<void>();
  int writes = 0;
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
    started.complete();
    await release.future;
    return super.write(key: key, value: value);
  }
}

class UncertainStorage extends FlutterSecureStorage {
  int writes = 0;
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
    await super.write(key: key, value: value);
    throw StateError('plugin response lost');
  }
}
