import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'package:flutter/services.dart';
import 'package:buzz/shared/hosted/hpke_backup.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:nostr/nostr.dart' as nostr;
import 'package:buzz/shared/deeplink/deep_link.dart';
import 'package:buzz/shared/hosted/account_identity_store.dart';
import 'package:buzz/shared/hosted/browser_auth.dart';
import 'package:buzz/shared/hosted/builderlab_account.dart';
import 'package:buzz/shared/hosted/hosted_identity_service.dart';
import 'package:buzz/shared/hosted/hosted_flow.dart';

class FixtureSeal extends HpkeBackup {
  FixtureSeal(this.envelope);
  final Map<String, dynamic> envelope;
  @override
  Future<Map<String, dynamic>> seal({
    required Map<String, dynamic> enrollment,
    required String secretHex,
  }) async {
    expect(nostr.Keys(secretHex).public, envelope['nostr_pubkey']);
    return envelope;
  }
}

class CancelledBrowser extends BrowserAuth {
  CancelledBrowser()
    : super(
        openBrowser: (_) async => throw PlatformException(code: 'cancelled'),
      );
  @override
  Future<void> initialize() async {}
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  setUp(() => FlutterSecureStorage.setMockInitialValues({}));
  test(
    'cancelling community sign-in returns to idle without admission',
    () async {
      final account = BuilderlabAccountClient();
      final browser = CancelledBrowser();
      final store = AccountIdentityStore();
      var admitted = false;
      final flow = HostedFlow(
        account: account,
        browser: browser,
        store: store,
        identity: HostedIdentityService(account: account, store: store),
        enterCommunity: (_, _, _, _) async {
          admitted = true;
        },
      );
      await flow.initialize();
      final idle = Completer<void>();
      flow.addListener(() {
        if (flow.stage == HostedFlowStage.idle && !idle.isCompleted) {
          idle.complete();
        }
      });
      await flow.start(
        const AddCommunityDeepLink(
          relayUrl: 'wss://example.communities.buzz.xyz',
          name: 'Wes',
        ),
      );
      await idle.future.timeout(const Duration(seconds: 2));
      expect(admitted, isFalse);
      expect(flow.problem, isNull);
      expect(await browser.retainedIntent(), isNull);
      flow.dispose();
      await browser.dispose();
      account.dispose();
    },
  );

  test(
    'real coordinator does not admit until scoped backup receipt exists',
    () async {
      final keys = nostr.Keys('1'.padLeft(64, '0'));
      final expiry = DateTime.now()
          .toUtc()
          .add(const Duration(hours: 1))
          .toIso8601String();
      final account = BuilderlabAccountClient(
        httpClient: MockClient((request) async {
          final data = request.url.path.endsWith('/exchange')
              ? {'session_credential': 'token', 'expires_at': expiry}
              : request.url.path.endsWith('/me')
              ? {'subject': 'owner', 'expires_at': expiry}
              : {
                  'identity': {'pubkey_hex': keys.public},
                };
          return http.Response(jsonEncode(data), 200);
        }),
      );
      await account.exchange(code: 'code', verifier: 'a' * 43);
      final store = AccountIdentityStore();
      await store.create(keys.nsec);
      final browser = BrowserAuth(openBrowser: (_) async => null);
      var admitted = 0;
      Completer<void>? admissionStarted;
      Completer<void>? admissionMayFinish;
      final flow = HostedFlow(
        account: account,
        browser: browser,
        store: store,
        identity: HostedIdentityService(account: account, store: store),
        enterCommunity: (target, subject, pubkey, isCurrent) async {
          expect(subject, 'owner');
          expect(pubkey, keys.public);
          admissionStarted?.complete();
          if (admissionMayFinish != null) await admissionMayFinish.future;
          if (isCurrent()) admitted++;
        },
      );
      await flow.start(
        const AddCommunityDeepLink(
          relayUrl: 'wss://example.communities.buzz.xyz',
          name: 'Wes',
        ),
      );
      expect(flow.problem, HostedFlowProblem.identityMismatch);
      expect(flow.canRetry, isFalse);
      expect(admitted, 0);
      await store.confirmBackup(
        BackupReceipt(
          backend: BuilderlabAccountClient.origin.toString(),
          subject: 'owner',
          pubkey: keys.public,
          backupId: 'receipt',
        ),
      );
      await flow.retry();
      expect(admitted, 1);
      expect(flow.stage, HostedFlowStage.complete);
      // A real admission request may finish after the user leaves setup.
      // Its durable write and the coordinator's completion must both be fenced.
      admissionStarted = Completer<void>();
      admissionMayFinish = Completer<void>();
      final pending = flow.retry();
      await admissionStarted.future;
      expect(flow.stage, HostedFlowStage.working);
      await flow.leave();
      admissionMayFinish.complete();
      await pending;
      expect(admitted, 1);
      expect(flow.stage, HostedFlowStage.idle);
      expect(await browser.retainedIntent(), isNull);
      flow.dispose();
      await browser.dispose();
      account.dispose();
    },
  );
  test(
    'real HTTP bind then backup reaches admission with persisted receipt',
    () async {
      final fixture =
          jsonDecode(
                File(
                  '../desktop/src-tauri/src/testdata/hpke_nsec_backup_v1.json',
                ).readAsStringSync(),
              )
              as Map<String, dynamic>;
      final envelope = Map<String, dynamic>.from(fixture['envelope'] as Map);
      final keys = nostr.Keys('42' * 32);
      final expiry = DateTime.now()
          .toUtc()
          .add(const Duration(hours: 1))
          .toIso8601String();
      var bound = false;
      final effects = <String>[];
      final account = BuilderlabAccountClient(
        httpClient: MockClient((request) async {
          final path = request.url.path;
          Object data;
          if (path.endsWith('/exchange')) {
            data = {'session_credential': 'token', 'expires_at': expiry};
          } else if (path.endsWith('/me')) {
            data = {'subject': 'owner', 'expires_at': expiry};
          } else if (path.endsWith('/current')) {
            data = {
              'identity': bound ? {'pubkey_hex': keys.public} : null,
            };
          } else if (path.endsWith('/status')) {
            data = {
              'current_nostr_pubkey': bound ? keys.public : null,
              'backup_nostr_pubkeys': <String>[],
            };
          } else if (path.endsWith('/challenge')) {
            effects.add('challenge');
            data = {
              'challenge_id': '12345678-1234-1234-1234-123456789abc',
              'nonce': 'a' * 43,
              'verification_code': '123456',
              'origin': BuilderlabAccountClient.origin.toString(),
              'expires_at': expiry,
            };
          } else if (path.endsWith('/verify')) {
            effects.add('verify');
            final body = jsonDecode(request.body) as Map;
            final event = jsonDecode(body['signed_payload'] as String) as Map;
            expect(event['kind'], 24243);
            expect(event['pubkey'], keys.public);
            bound = true;
            data = {
              'identity': {'pubkey_hex': keys.public},
            };
          } else if (path.endsWith('/enrollment')) {
            effects.add('enrollment');
            expect(bound, isTrue);
            data = envelope;
          } else if (path.endsWith('/nsec-backups')) {
            effects.add('upload');
            data = {'stored': true, 'backup_id': envelope['backup_id']};
          } else {
            throw StateError('Unexpected route');
          }
          return http.Response(jsonEncode(data), 200);
        }),
      );
      await account.exchange(code: 'code', verifier: 'a' * 43);
      final store = AccountIdentityStore();
      await store.create(keys.nsec);
      final browser = BrowserAuth(openBrowser: (_) async => null);
      final flow = HostedFlow(
        account: account,
        browser: browser,
        store: store,
        identity: HostedIdentityService(account: account, store: store),
        hpke: FixtureSeal(envelope),
        enterCommunity: (target, subject, pubkey, isCurrent) async {
          expect(isCurrent(), isTrue);
          expect(
            (await store.read())!.hasBackupFor(
              backend: BuilderlabAccountClient.origin.toString(),
              subject: 'owner',
            ),
            isTrue,
          );
          effects.add('admitted');
        },
      );
      await flow.start(
        const AddCommunityDeepLink(
          relayUrl: 'wss://example.communities.buzz.xyz',
          name: 'Wes',
        ),
      );
      expect(flow.stage, HostedFlowStage.complete);
      expect(effects, [
        'challenge',
        'verify',
        'enrollment',
        'upload',
        'admitted',
      ]);
      flow.dispose();
      await browser.dispose();
      account.dispose();
    },
  );
}
