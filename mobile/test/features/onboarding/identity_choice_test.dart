import 'dart:convert';

import 'package:buzz/features/onboarding/hosted_connect_page.dart';
import 'package:buzz/hosted_app_gate.dart';
import 'package:buzz/shared/deeplink/deep_link.dart';
import 'package:buzz/shared/deeplink/pending_deep_link_provider.dart';
import 'package:buzz/shared/hosted/account_identity_store.dart';
import 'package:buzz/shared/hosted/browser_auth.dart';
import 'package:buzz/shared/hosted/device_identity_source.dart';
import 'package:buzz/shared/hosted/hosted_flow.dart';
import 'package:buzz/shared/hosted/hosted_identity_service.dart';
import 'package:flutter/material.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';
import 'package:nostr/nostr.dart' as nostr;

import '../../shared/hosted/device_identity_reconciliation_test.dart'
    show InventoryAccount;

class _Browser extends BrowserAuth {
  @override
  Future<void> initialize() async {}
  @override
  Future<Map<String, String>?> retainedIntent() async => null;
  @override
  Future<void> clearIntent() async {}
  @override
  Future<void> cancel() async {}
}

void main() {
  testWidgets(
    'inline link flow shows identity choice; leave and explicit selection preserve keys',
    (tester) async {
      final first = nostr.Keys('1'.padLeft(64, '0'));
      final second = nostr.Keys('2'.padLeft(64, '0'));
      final raw = jsonEncode([
        {'name': 'First community', 'nsec': first.nsec, 'pubkey': first.public},
        {
          'name': 'Second community',
          'nsec': second.nsec,
          'pubkey': second.public,
        },
      ]);
      FlutterSecureStorage.setMockInitialValues({'buzz_communities': raw});
      PendingDeepLinkNotifier.debugUriStreamOverride =
          const Stream<Uri>.empty();
      final account = InventoryAccount();
      final store = AccountIdentityStore();
      final browser = _Browser();
      var admitted = false;
      final flow = HostedFlow(
        account: account,
        browser: browser,
        store: store,
        identity: HostedIdentityService(
          account: account,
          store: store,
          deviceIdentities: DeviceIdentitySource(),
        ),
        enterCommunity: (_, _, _, _) async {
          admitted = true;
        },
      );
      final container = ProviderContainer(
        overrides: [hostedFlowProvider.overrideWithValue(flow)],
      );
      addTearDown(() async {
        PendingDeepLinkNotifier.debugUriStreamOverride = null;
        container.dispose();
        flow.dispose();
        await browser.dispose();
        account.dispose();
      });
      container.read(hostedInlineEntryProvider.notifier).setActive(true);
      await tester.pumpWidget(
        UncontrolledProviderScope(
          container: container,
          child: const MaterialApp(
            home: HostedAppGate(
              authenticated: false,
              child: Scaffold(body: Text('Link form')),
            ),
          ),
        ),
      );
      await tester.pumpAndSettle();
      const target = AddCommunityDeepLink(
        relayUrl: 'wss://team.communities.buzz.xyz',
        name: 'Team',
      );
      await flow.start(target);
      await tester.pumpAndSettle();
      expect(find.byType(HostedConnectPage), findsOneWidget);
      expect(find.text('First community'), findsOneWidget);
      expect(find.text('Second community'), findsOneWidget);
      expect(await store.read(), isNull);
      await tester.tap(find.text('Leave setup'));
      await tester.pumpAndSettle();
      expect(flow.stage, HostedFlowStage.idle);
      expect(await store.read(), isNull);
      await flow.start(target);
      await tester.pumpAndSettle();
      await tester.tap(find.text('Second community'));
      await tester.pumpAndSettle();
      expect((await store.read())!.pubkey, second.public);
      expect(
        await const FlutterSecureStorage().read(key: 'buzz_communities'),
        raw,
      );
      expect(admitted, isFalse);
      // Fixture refuses binding: choosing a key never bypasses remaining gates.
      expect(flow.problem, HostedFlowProblem.denied);
      expect(
        account.calls.where((path) => path.endsWith('/challenge')).length,
        1,
      );
      expect(tester.takeException(), isNull);
    },
  );
}
