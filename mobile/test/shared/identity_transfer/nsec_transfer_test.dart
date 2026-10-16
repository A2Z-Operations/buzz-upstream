import 'package:buzz/shared/community/community.dart';
import 'package:buzz/shared/hosted/account_identity_store.dart';
import 'package:buzz/shared/identity_transfer/nsec_transfer.dart';
import 'package:buzz/shared/security/sensitive_action_authorizer.dart';
import 'dart:convert';
import 'package:file_selector/file_selector.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:nostr/nostr.dart' as nostr;

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  final keys = nostr.Keys('1'.padLeft(64, '0'));
  final other = nostr.Keys('2'.padLeft(64, '0'));
  setUp(() => FlutterSecureStorage.setMockInitialValues({}));

  for (final platform in [TargetPlatform.android, TargetPlatform.iOS]) {
    test(
      'native $platform import validates text and preserves cancellation/error',
      () async {
        debugDefaultTargetPlatformOverride = platform;
        const channel = MethodChannel('buzz/identity_file');
        final messenger =
            TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
        addTearDown(() {
          debugDefaultTargetPlatformOverride = null;
          messenger.setMockMethodCallHandler(channel, null);
        });
        String? response = '${keys.nsec}\n';
        var failRead = false;
        messenger.setMockMethodCallHandler(channel, (call) async {
          expect(call.method, 'import');
          expect(call.arguments, isNull);
          if (failRead) throw PlatformException(code: 'import_failed');
          return response;
        });
        expect(await NsecTransfer.pickFile(), keys.nsec);
        response = null;
        expect(await NsecTransfer.pickFile(), isNull);
        response = keys.public;
        await expectLater(
          NsecTransfer.pickFile(),
          throwsA(AccountIdentityFailure.invalidKey),
        );
        response = 'x' * 1025;
        await expectLater(
          NsecTransfer.pickFile(),
          throwsA(AccountIdentityFailure.invalidKey),
        );
        failRead = true;
        await expectLater(
          NsecTransfer.pickFile(),
          throwsA(isA<PlatformException>()),
        );
      },
    );
  }

  test('paste parsing rejects wrong key type, invalid scalar and checksum', () {
    expect(NsecTransfer.preview('  ${keys.nsec}\n'), keys.public);
    for (final value in [
      keys.public,
      'nsec1bad',
      nostr.Nip19.encode(prefix: nostr.Nip19Prefix.nsec, data: '0' * 64),
    ]) {
      expect(
        () => NsecTransfer.preview(value),
        throwsA(AccountIdentityFailure.invalidKey),
      );
    }
  });

  test('file import validates size, UTF-8, and key type', () async {
    XFile file(List<int> bytes) => XFile.fromData(Uint8List.fromList(bytes));
    expect(
      await NsecTransfer.readFile(file(utf8.encode('${keys.nsec}\n'))),
      keys.nsec,
    );
    for (final bytes in [
      utf8.encode('x' * 1025),
      [0xff],
      utf8.encode(keys.public),
    ]) {
      await expectLater(
        NsecTransfer.readFile(file(bytes)),
        throwsA(AccountIdentityFailure.invalidKey),
      );
    }
  });

  test(
    'manual import is create-only, preserves receipt and occupied keys',
    () async {
      final store = AccountIdentityStore();
      final receipt = BackupReceipt(
        backend: 'https://app.builderlab.xyz',
        subject: 'a',
        pubkey: keys.public,
        backupId: 'backup',
      );
      await store.create(keys.nsec, recoveredBackup: receipt);
      final restored = await NsecTransfer.importAccount(
        ' ${keys.nsec}\n',
        store,
      );
      expect(restored.backup?.backupId, 'backup');
      await expectLater(
        NsecTransfer.importAccount(other.nsec, store),
        throwsA(AccountIdentityFailure.occupied),
      );
      expect(await store.withKeys((k) async => k.public), keys.public);
      expect((await store.read())?.backup?.backupId, 'backup');
    },
  );

  test(
    'manual import fences cancelled operations before secure write',
    () async {
      final store = AccountIdentityStore();
      await expectLater(
        NsecTransfer.importAccount(
          keys.nsec,
          store,
          checkCurrent: () => throw StateError('cancelled'),
        ),
        throwsStateError,
      );
      expect(await store.read(), isNull);
    },
  );

  test(
    'export respects biometric policy, auth denial and selected identity',
    () async {
      final community = Community.create(
        name: 'Test',
        relayUrl: 'wss://test.invalid',
        nsec: keys.nsec,
        pubkey: keys.public,
        sensitiveActionPolicy: SensitiveActionPolicy.enabled,
      );
      var shares = 0;
      var current = true;
      var result = DeviceAuthResult.cancelled;
      Future<bool> export() => NsecTransfer.exportCommunity(
        community: community,
        authorize: (biometricOnly) async {
          expect(biometricOnly, isTrue);
          return result;
        },
        isCurrent: () => current,
        share: (nsec) async {
          expect(nsec, keys.nsec);
          shares++;
          return true;
        },
      );
      expect(await export(), isFalse);
      result = DeviceAuthResult.success;
      current = false;
      expect(await export(), isFalse);
      expect(shares, 0);
      current = true;
      expect(await export(), isTrue);
      expect(shares, 1);
      await expectLater(
        NsecTransfer.exportCommunity(
          community: community.copyWith(nsec: other.nsec),
          authorize: (_) async =>
              fail('must not authorize a mismatched identity'),
          isCurrent: () => true,
          share: (_) async => fail('must not share a mismatched identity'),
        ),
        throwsA(AccountIdentityFailure.mismatch),
      );
    },
  );
}
