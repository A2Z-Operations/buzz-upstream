import 'dart:async';
import 'dart:convert';

import 'package:buzz/shared/deeplink/deep_link.dart';
import 'package:buzz/shared/hosted/account_identity_store.dart';
import 'package:buzz/shared/hosted/builderlab_account.dart';
import 'package:buzz/shared/hosted/hosted_admission.dart';
import 'package:buzz/shared/hosted/hosted_join_policy.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:nostr/nostr.dart' as nostr;

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  final keys = nostr.Keys(
    '0000000000000000000000000000000000000000000000000000000000000001',
  );
  const invite = InviteDeepLink(
    relayUrl: 'wss://example.communities.buzz.xyz',
    code: 'invite',
    policyReceipt: 'policy',
  );
  late _Account account;
  late AccountIdentityStore store;
  late List<http.Request> claims;
  late HostedAdmission admission;
  setUp(() {
    FlutterSecureStorage.setMockInitialValues({});
    account = _Account(keys.public);
    store = AccountIdentityStore();
    claims = [];
    admission = HostedAdmission(
      account: account,
      store: store,
      probe: (_) async {},
      client: MockClient((request) async {
        if (request.method == 'POST') claims.add(request);
        return http.Response('{}', 200);
      }),
    );
  });
  tearDown(() {
    admission.dispose();
    account.dispose();
  });
  Future<void> backup() async => store.confirmBackup(
    BackupReceipt(
      backend: BuilderlabAccountClient.origin.origin,
      subject: 'owner',
      pubkey: keys.public,
      backupId: 'backup',
    ),
  );

  test('no claim without scoped backup or current matching binding', () async {
    await store.create(keys.nsec);
    await expectLater(
      admission.prepare(invite, 'owner', keys.public),
      throwsA(AccountFailure.denied),
    );
    expect(account.bindingReads, 0);
    expect(claims, isEmpty);
    await backup();
    account.bound = 'a' * 64;
    await expectLater(
      admission.prepare(invite, 'owner', keys.public),
      throwsA(AccountFailure.denied),
    );
    expect(account.bindingReads, 1);
    expect(claims, isEmpty);
  });

  test('claim signature and returned identity use resolved key', () async {
    await store.create(keys.nsec);
    await backup();
    final community = await admission.prepare(invite, 'owner', keys.public);
    expect(claims, hasLength(1));
    final request = claims.single;
    final auth = request.headers['Authorization']!;
    expect(auth.startsWith('Nostr '), isTrue);
    final event = nostr.Event.fromJson(
      utf8.decode(base64.decode(auth.substring(6))),
      verify: true,
    );
    expect(event.pubkey, keys.public);
    expect(event.kind, 27235);
    expect(
      event.tags.map((tag) => tag.toList()).toList(),
      contains(
        equals(['u', 'https://example.communities.buzz.xyz/api/invites/claim']),
      ),
    );
    expect(
      event.tags.map((tag) => tag.toList()).toList(),
      contains(equals(['method', 'POST'])),
    );
    expect(jsonDecode(request.body), {'code': 'invite'});
    expect(community.pubkey, keys.public);
    expect(community.nsec, keys.nsec);
    expect(request.followRedirects, isFalse);
  });

  test(
    'fresh policy consent replaces stale web receipt before signed claim',
    () async {
      await store.create(keys.nsec);
      await backup();
      var confirmed = false;
      final paths = <String>[];
      admission.dispose();
      admission = HostedAdmission(
        account: account,
        store: store,
        probe: (_) async {},
        confirmPolicy: (policy) async {
          expect(policy.version, 'v2');
          expect(policy.ageRequired, isTrue);
          expect(policy.terms, 'New terms');
          confirmed = true;
          return true;
        },
        client: MockClient((request) async {
          paths.add(request.url.path);
          expect(request.followRedirects, isFalse);
          if (request.method == 'GET') {
            return http.Response(
              jsonEncode({
                'policy': {
                  'version': 'v2',
                  'age_attestation_required': true,
                  'terms_markdown': 'New terms',
                },
              }),
              200,
            );
          }
          expect(confirmed, isTrue);
          if (request.url.path.endsWith('accept-policy')) {
            expect(jsonDecode(request.body), {
              'code': 'invite',
              'policy_version': 'v2',
              'age_confirmed': true,
            });
            return http.Response('{"receipt":"fresh"}', 200);
          }
          expect(jsonDecode(request.body), {
            'code': 'invite',
            'policy_receipt': 'fresh',
          });
          expect(request.headers['Authorization'], startsWith('Nostr '));
          return http.Response('{}', 200);
        }),
      );
      await admission.prepare(invite, 'owner', keys.public);
      expect(paths, [
        '/api/join-policy',
        '/api/invites/accept-policy',
        '/api/invites/claim',
      ]);
    },
  );

  test('declined or stale consent cannot accept or claim an invite', () async {
    await store.create(keys.nsec);
    await backup();
    for (final stale in [false, true]) {
      var current = true;
      admission.dispose();
      admission = HostedAdmission(
        account: account,
        store: store,
        probe: (_) async {},
        confirmPolicy: (_) async {
          if (stale) current = false;
          return stale;
        },
        client: MockClient((request) async {
          expect(request.method, 'GET');
          return http.Response(
            '{"policy":{"version":"v1","age_attestation_required":false}}',
            200,
          );
        }),
      );
      await expectLater(
        admission.prepare(
          invite,
          'owner',
          keys.public,
          isCurrent: () => current,
        ),
        throwsA(
          stale
              ? AccountFailure.unavailable
              : HostedInviteFailure.policyRequired,
        ),
      );
    }
  });

  test('invite rejection reads only supported error codes', () async {
    await store.create(keys.nsec);
    await backup();
    for (final entry in {
      'invite_expired': HostedInviteFailure.expired,
      'invite_exhausted': HostedInviteFailure.exhausted,
      'invite_invalid': HostedInviteFailure.invalid,
      'join_policy_required': HostedInviteFailure.policyRequired,
    }.entries) {
      admission.dispose();
      admission = HostedAdmission(
        account: account,
        store: store,
        probe: (_) async {},
        client: MockClient(
          (request) async => request.method == 'GET'
              ? http.Response('{}', 200)
              : http.Response(jsonEncode({'error': entry.key}), 403),
        ),
      );
      await expectLater(
        admission.prepare(invite, 'owner', keys.public),
        throwsA(entry.value),
      );
    }
  });

  test('cancellation during binding prevents external claim', () async {
    await store.create(keys.nsec);
    await backup();
    var current = true;
    account.onBinding = () => current = false;
    await expectLater(
      admission.prepare(invite, 'owner', keys.public, isCurrent: () => current),
      throwsA(AccountFailure.unavailable),
    );
    expect(claims, isEmpty);
  });

  test('cancelled or rejected relay probe never returns community', () async {
    await store.create(keys.nsec);
    await backup();
    final pending = Completer<void>();
    final started = Completer<void>();
    var current = true;
    final delayed = HostedAdmission(
      account: account,
      store: store,
      probe: (_) {
        started.complete();
        return pending.future;
      },
    );
    const link = AddCommunityDeepLink(
      relayUrl: 'wss://example.communities.buzz.xyz',
      name: 'Wes',
    );
    final result = delayed.prepare(
      link,
      'owner',
      keys.public,
      isCurrent: () => current,
    );
    final expectation = expectLater(
      result,
      throwsA(AccountFailure.unavailable),
    );
    await started.future;
    current = false;
    pending.complete();
    await expectation;
    delayed.dispose();
    final rejected = HostedAdmission(
      account: account,
      store: store,
      probe: (_) async {
        throw AccountFailure.denied;
      },
    );
    await expectLater(
      rejected.prepare(link, 'owner', keys.public),
      throwsA(AccountFailure.denied),
    );
    rejected.dispose();
    expect(claims, isEmpty);
  });

  test('different account and untrusted host cannot claim', () async {
    await store.create(keys.nsec);
    await backup();
    await expectLater(
      admission.prepare(invite, 'other', keys.public),
      throwsA(AccountFailure.differentAccount),
    );
    await expectLater(
      admission.prepare(
        const InviteDeepLink(
          relayUrl: 'wss://communities.buzz.xyz.attacker.test',
          code: 'i',
        ),
        'owner',
        keys.public,
      ),
      throwsA(AccountFailure.denied),
    );
    expect(claims, isEmpty);
  });
}

class _Account extends BuilderlabAccountClient {
  _Account(this.bound);
  String bound;
  int bindingReads = 0;
  void Function()? onBinding;
  @override
  BuilderlabAccount get account =>
      BuilderlabAccount(subject: 'owner', expiresAt: DateTime.utc(2100));
  @override
  Future<Map<String, dynamic>> currentIdentity() async {
    bindingReads++;
    onBinding?.call();
    return {
      'identity': {'pubkey_hex': bound},
    };
  }
}
