import 'package:buzz/hosted_app_gate.dart';
import 'package:buzz/shared/auth/auth.dart';
import 'package:buzz/shared/deeplink/deep_link.dart';
import 'package:buzz/shared/deeplink/pending_deep_link_provider.dart';
import 'package:buzz/shared/hosted/builderlab_account.dart';
import 'package:buzz/shared/hosted/hosted_flow.dart';
import 'package:flutter/material.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';

import '../../helpers/hosted_communities_fixture.dart';
import '../../helpers/widget_helpers.dart';

void main() {
  testWidgets(
    'reopened hosted invite with pending starters routes through setup',
    (tester) async {
      FlutterSecureStorage.setMockInitialValues({});
      PendingDeepLinkNotifier.debugUriStreamOverride =
          const Stream<Uri>.empty();
      final f = HostedCommunitiesFixture();
      await f.initialize(widget: true);
      final storage = CommunityStorage();
      final pending = Community.create(
        name: 'Pending',
        relayUrl: 'wss://pending.communities.buzz.xyz',
        pubkey: f.keys.public,
        nsec: f.keys.nsec,
        starterSetupIncomplete: true,
      );
      await storage.save(pending);
      final entered = <BuzzDeepLink>[];
      final flow = HostedFlow(
        account: f.account,
        browser: f.browser,
        store: f.store,
        identity: f.flow.identity,
        enterCommunity: (link, _, _, _) async {
          entered.add(link);
          throw AccountFailure.unavailable;
        },
      );
      addTearDown(() async {
        PendingDeepLinkNotifier.debugUriStreamOverride = null;
        flow.dispose();
        await f.dispose();
      });
      await tester.pumpWidget(
        WidgetHelpers.testable(
          disableAnimations: true,
          overrides: [hostedFlowProvider.overrideWithValue(flow)],
          child: const HostedAppGate(
            authenticated: false,
            child: SizedBox.expand(),
          ),
        ),
      );
      await tester.pumpAndSettle();
      final container = ProviderScope.containerOf(
        tester.element(find.byType(HostedAppGate)),
      );
      container
          .read(pendingDeepLinkProvider.notifier)
          .open(
            Uri(
              scheme: 'buzz',
              host: 'join',
              queryParameters: {
                'relay': pending.relayUrl,
                'code': 'already-consumed',
              },
            ),
          );
      await tester.pumpAndSettle();
      expect(entered.single, isA<InviteDeepLink>());
      expect((entered.single as InviteDeepLink).code, 'already-consumed');
      expect(flow.stage, HostedFlowStage.blocked);
      expect(container.read(communityEntryCompletionProvider), 0);
      expect((await storage.loadAll()).single.starterSetupIncomplete, isTrue);
      expect(find.text('Try again').hitTestable(), findsOneWidget);
      await tester.pumpWidget(const SizedBox.shrink());
      await tester.pump();
    },
  );
}
