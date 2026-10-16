import 'package:buzz/features/onboarding/hosted_connect_page.dart';
import 'package:buzz/hosted_app_gate.dart';
import 'package:buzz/onboarding_app_entry.dart';
import 'package:buzz/shared/deeplink/pending_deep_link_provider.dart';
import 'package:buzz/shared/hosted/hosted_flow.dart';
import 'package:buzz/shared/hosted/builderlab_account.dart';
import 'package:buzz/shared/hosted/hosted_community_connection.dart';
import 'package:flutter/material.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';
import 'package:buzz/shared/deeplink/deep_link.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';

import '../../helpers/hosted_communities_fixture.dart';
import '../../helpers/widget_helpers.dart';

void main() {
  testWidgets(
    'welcome discovery selection and empty-state handoff use the real flow',
    (tester) async {
      // Keep the process-wide secure-store queue in one widget-test zone.
      {
        FlutterSecureStorage.setMockInitialValues({});
        PendingDeepLinkNotifier.debugUriStreamOverride =
            const Stream<Uri>.empty();
        final f = HostedCommunitiesFixture();
        await f.initialize(widget: true);
        f.rows = [f.row('one'), f.row('two')];
        addTearDown(() async {
          PendingDeepLinkNotifier.debugUriStreamOverride = null;
          await f.dispose();
        });
        await tester.pumpWidget(
          WidgetHelpers.testable(
            disableAnimations: true,
            child: const HostedAppGate(
              authenticated: false,
              child: OnboardingAppEntry(),
            ),
            overrides: [hostedFlowProvider.overrideWithValue(f.flow)],
          ),
        );
        await tester.pumpAndSettle();
        expect(find.text('Continue with Builderlab'), findsOneWidget);
        expect(find.byType(HostedConnectPage), findsNothing);
        await tester.tap(find.text('Continue with Builderlab'));
        await tester.pumpAndSettle();
        expect(find.text('Choose your communities'), findsOneWidget);
        expect(
          tester
              .widgetList<CheckboxListTile>(find.byType(CheckboxListTile))
              .every((tile) => tile.value == true),
          isTrue,
        );
        await tester.tap(find.text('one'));
        await tester.pumpAndSettle();
        await tester.tap(find.text('Continue').hitTestable());
        await tester.pumpAndSettle();
        expect(f.admitted, ['two']);
        expect(find.byType(HostedConnectPage), findsNothing);
        f.admitted.clear();
        f.beforeAdmission = (name) async {
          if (name == 'two') throw const SavedCommunityIdentityConflict();
        };
        await tester.tap(find.text('Continue with Builderlab'));
        await tester.pumpAndSettle();
        await tester.tap(find.text('Continue').hitTestable());
        await tester.pumpAndSettle();
        expect(find.text('Keep saved identity and skip'), findsOneWidget);
        expect(find.text('1 community added and kept.'), findsOneWidget);
        await tester.tap(find.text('Keep saved identity and skip'));
        await tester.pumpAndSettle();
        expect(f.admitted, ['one']);
        expect(find.byType(HostedConnectPage), findsNothing);
        f.admitted.clear();
        f.beforeAdmission = (name) async {
          if (name == 'two') throw AccountFailure.rejected;
        };
        await tester.tap(find.text('Continue with Builderlab'));
        await tester.pumpAndSettle();
        await tester.tap(find.text('Continue').hitTestable());
        await tester.pumpAndSettle();
        expect(find.text('1 community added and kept.'), findsOneWidget);
        expect(find.text('Try again').hitTestable(), findsNothing);
        await tester.tap(find.text('Back to choices'));
        await tester.pumpAndSettle();
        expect(find.byType(CheckboxListTile), findsOneWidget);
        expect(find.text('two'), findsOneWidget);
      }
      await tester.pumpWidget(const SizedBox.shrink());
      {
        FlutterSecureStorage.setMockInitialValues({});
        PendingDeepLinkNotifier.debugUriStreamOverride =
            const Stream<Uri>.empty();
        final f = HostedCommunitiesFixture();
        await f.initialize(widget: true);
        addTearDown(() async {
          PendingDeepLinkNotifier.debugUriStreamOverride = null;
          await f.dispose();
        });
        await f.flow.signInAccount();
        await tester.pumpWidget(
          WidgetHelpers.testable(
            disableAnimations: true,
            child: HostedConnectPage(flow: f.flow, onClose: f.flow.leave),
          ),
        );
        await tester.pumpAndSettle();
        expect(find.text('Create a community in Builderlab'), findsOneWidget);
        await tester.tap(find.text('Create a community in Builderlab'));
        await tester.pumpAndSettle();
        expect(f.opened?.path, '/buzz');
        expect(find.text('Refresh communities'), findsOneWidget);
        await tester.enterText(find.byType(TextField), 'not a valid url');
        await tester.tap(find.text('Open community link'));
        await tester.pumpAndSettle();
        expect(
          find.text('Enter a valid community address or invite link.'),
          findsOneWidget,
        );
        await tester.enterText(
          find.byType(TextField),
          'https://one.communities.buzz.xyz/invite/code',
        );
        await tester.testTextInput.receiveAction(TextInputAction.go);
        await tester.pumpAndSettle();
        expect(f.flow.stage, HostedFlowStage.idle);
        final container = ProviderScope.containerOf(
          tester.element(find.byType(HostedConnectPage)),
          listen: false,
        );
        expect(container.read(pendingDeepLinkProvider), isA<InviteDeepLink>());
      }
    },
  );
}
