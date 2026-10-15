import 'dart:convert';

import 'package:buzz/features/onboarding/hosted_connect_page.dart';
import 'package:buzz/hosted_app_gate.dart';
import 'package:buzz/onboarding_app_entry.dart';
import 'package:buzz/shared/community/community.dart';
import 'package:buzz/shared/deeplink/pending_deep_link_provider.dart';
import 'package:buzz/shared/hosted/account_identity_store.dart';
import 'package:buzz/shared/hosted/browser_auth.dart';
import 'package:buzz/shared/hosted/device_identity_source.dart';
import 'package:buzz/shared/hosted/hosted_flow.dart';
import 'package:buzz/shared/hosted/hosted_identity_service.dart';
import 'package:flutter/material.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:nostr/nostr.dart' as nostr;

import '../../helpers/widget_helpers.dart';
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
    'real link form supports local choice and cloud consent, leave and resubmit',
    (tester) async {
      // One widget zone matches the process-wide secure-store writer queue.
      for (final cloud in [false, true]) {
        final first = nostr.Keys('1'.padLeft(64, '0'));
        final second = nostr.Keys('2'.padLeft(64, '0'));
        final raw = jsonEncode([
          Community.create(
            name: 'First community',
            relayUrl: 'wss://first.example.invalid',
            nsec: first.nsec,
            pubkey: first.public,
          ).toJson(),
          if (!cloud)
            Community.create(
              name: 'Second community',
              relayUrl: 'wss://second.example.invalid',
              nsec: second.nsec,
              pubkey: second.public,
            ).toJson(),
        ]);
        FlutterSecureStorage.setMockInitialValues({'buzz_communities': raw});
        PendingDeepLinkNotifier.debugUriStreamOverride =
            const Stream<Uri>.empty();
        final account = InventoryAccount();
        if (cloud) {
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
        }
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
        addTearDown(() async {
          PendingDeepLinkNotifier.debugUriStreamOverride = null;
          flow.dispose();
          await browser.dispose();
          account.dispose();
        });
        await tester.pumpWidget(
          WidgetHelpers.testable(
            disableAnimations: true,
            overrides: [hostedFlowProvider.overrideWithValue(flow)],
            child: const HostedAppGate(
              authenticated: false,
              child: OnboardingAppEntry(),
            ),
          ),
        );
        await tester.pumpAndSettle();
        await tester.tap(find.text('Enter a community link').hitTestable());
        await tester.pumpAndSettle();
        const url = 'https://team.communities.buzz.xyz';
        final input = find.byKey(const Key('community-link-input'));
        await tester.enterText(input, url);
        await tester.testTextInput.receiveAction(TextInputAction.go);
        await tester.pumpAndSettle();
        for (
          var attempt = 0;
          attempt < 30 && flow.stage != HostedFlowStage.chooseIdentity;
          attempt++
        ) {
          await tester.pump(const Duration(milliseconds: 50));
        }
        expect(
          flow.stage,
          HostedFlowStage.chooseIdentity,
          reason: 'Calls: ${account.calls}',
        );
        expect(find.byType(HostedConnectPage), findsOneWidget);
        expect(
          find.textContaining(
            'This app cannot change the selected Builderlab identity later.',
          ),
          findsOneWidget,
        );
        if (cloud) {
          expect(
            find.textContaining(
              'It differs from your saved community identities.',
            ),
            findsOneWidget,
          );
          expect(find.text('Use Builderlab identity'), findsOneWidget);
        } else {
          expect(find.text('First community'), findsOneWidget);
          expect(find.text('Second community'), findsOneWidget);
        }
        expect(await store.read(), isNull);
        await tester.ensureVisible(find.text('Leave setup'));
        await tester.tap(find.text('Leave setup'));
        await tester.pumpAndSettle();
        expect(flow.stage, HostedFlowStage.idle);
        expect(flow.identityChoices, isEmpty);
        expect(await store.read(), isNull);
        expect(tester.widget<TextField>(input).controller!.text, url);
        expect(tester.widget<TextField>(input).enabled, isTrue);
        await tester.ensureVisible(find.text('Continue'));
        await tester.pumpAndSettle();
        await tester.tap(find.text('Continue').hitTestable());
        await tester.pumpAndSettle();
        final choice = find.text(
          cloud ? 'Use Builderlab identity' : 'Second community',
        );
        await tester.ensureVisible(choice);
        await tester.tap(choice);
        await tester.pumpAndSettle();
        expect((await store.read())!.pubkey, second.public);
        expect(
          await const FlutterSecureStorage().read(key: 'buzz_communities'),
          raw,
        );
        expect(flow.identityChoices, isEmpty);
        expect(admitted, cloud);
        if (!cloud) {
          // The fixture refuses binding: a choice never bypasses later gates.
          expect(flow.problem, HostedFlowProblem.denied);
          expect(
            find.text('This account does not have access.'),
            findsOneWidget,
          );
          expect(
            account.calls.where((path) => path.endsWith('/challenge')).length,
            1,
          );
        }
        expect(tester.takeException(), isNull);
        await tester.pumpWidget(const SizedBox.shrink());
        await tester.pumpAndSettle();
      }
    },
  );
}
