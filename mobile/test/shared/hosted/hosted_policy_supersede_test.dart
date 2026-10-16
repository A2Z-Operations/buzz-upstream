import 'dart:convert';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:buzz/shared/hosted/hosted_admission.dart';
import 'package:buzz/shared/hosted/hosted_identity_service.dart';
import 'dart:async';
import 'package:buzz/shared/deeplink/deep_link.dart';
import 'package:buzz/shared/hosted/hosted_flow.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';
import '../../helpers/hosted_communities_fixture.dart';

void main() {
  test(
    'a superseding invite cancels queued consent and reaches admission',
    () async {
      TestWidgetsFlutterBinding.ensureInitialized();
      FlutterSecureStorage.setMockInitialValues({});
      final f = HostedCommunitiesFixture();
      await f.initialize();
      addTearDown(f.dispose);
      final prompted = Completer<void>();
      final requests = <http.Request>[];
      final admitted = <String>[];
      late final HostedFlow flow;
      final admission = HostedAdmission(
        account: f.account,
        store: f.store,
        probe: (_) async {},
        confirmPolicy: (policy) {
          final result = flow.confirmJoinPolicy(policy);
          prompted.complete();
          return result;
        },
        client: MockClient((request) async {
          requests.add(request);
          if (request.url.host.startsWith('first.')) {
            return http.Response(
              jsonEncode({
                'policy': {'version': 'v1', 'age_attestation_required': true},
              }),
              200,
            );
          }
          return http.Response('{}', 200);
        }),
      );
      flow = HostedFlow(
        account: f.account,
        browser: f.browser,
        store: f.store,
        identity: HostedIdentityService(account: f.account, store: f.store),
        enterCommunity: (link, subject, pubkey, current) async {
          final community = await admission.prepare(
            link,
            subject,
            pubkey,
            isCurrent: current,
          );
          if (current()) admitted.add(community.relayUrl);
        },
      );
      addTearDown(() {
        flow.dispose();
        admission.dispose();
      });
      final first = flow.start(
        const InviteDeepLink(
          relayUrl: 'wss://first.communities.buzz.xyz',
          code: 'first',
        ),
      );
      await prompted.future.timeout(const Duration(seconds: 2));
      final second = flow.start(
        const InviteDeepLink(
          relayUrl: 'wss://second.communities.buzz.xyz',
          code: 'second',
        ),
      );
      await Future.wait([first, second]).timeout(const Duration(seconds: 2));
      expect(
        requests.where(
          (r) => r.url.host.startsWith('first.') && r.method == 'POST',
        ),
        isEmpty,
      );
      expect(flow.stage, HostedFlowStage.complete);
      expect(admitted, contains('wss://second.communities.buzz.xyz'));
    },
  );
}
