import 'dart:async';
import 'package:buzz/hosted_app_gate.dart';
import 'package:buzz/shared/hosted/hosted_flow.dart';

import 'package:buzz/features/pairing/pairing_page.dart';
import 'package:buzz/shared/deeplink/deep_link.dart';
import 'package:buzz/shared/deeplink/pending_deep_link_provider.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';

import '../../helpers/widget_helpers.dart';

void main() {
  testWidgets('link panel validates, dispatches an invite, and slides back', (
    tester,
  ) async {
    PendingDeepLinkNotifier.debugUriStreamOverride = const Stream<Uri>.empty();
    addTearDown(() => PendingDeepLinkNotifier.debugUriStreamOverride = null);
    var entering = false;
    await tester.pumpWidget(
      WidgetHelpers.testable(
        child: StatefulBuilder(
          builder: (context, setState) => PairingPage(
            connectionLanding: true,
            enteringCommunityLink: entering,
            onCommunityLink: () => setState(() => entering = true),
            onWelcomeBack: () => setState(() => entering = false),
          ),
        ),
      ),
    );
    final logo = find.byKey(const Key('pairing-buzz-wordmark'));
    final position = tester.getTopLeft(logo);
    await tester.tap(find.text('Enter a community link').hitTestable());
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 250));
    expect(tester.getTopLeft(logo), position);
    await tester.tap(find.text('Continue').hitTestable());
    await tester.pump();
    expect(
      find.text('Enter a valid community address or invite link.'),
      findsOneWidget,
    );
    await tester.enterText(
      find.byKey(const Key('community-link-input')),
      'https://team.example.com/invite/abc123',
    );
    await tester.tap(find.text('Continue').hitTestable());
    await tester.pump();
    final container = ProviderScope.containerOf(
      tester.element(find.byType(PairingPage)),
    );
    final link = container.read(pendingDeepLinkProvider) as InviteDeepLink;
    expect(link.relayUrl, 'wss://team.example.com');
    expect(link.code, 'abc123');
    final flow = container.read(hostedFlowProvider);
    flow.accountOnly = false;
    flow.stage = HostedFlowStage.signIn;
    flow.notifyListeners();
    await tester.pump();
    expect(find.text('Connecting…'), findsOneWidget);
    expect(
      tester
          .widget<TextField>(find.byKey(const Key('community-link-input')))
          .controller!
          .text,
      'https://team.example.com/invite/abc123',
    );
    flow.stage = HostedFlowStage.blocked;
    flow.problem = HostedFlowProblem.retry;
    flow.notifyListeners();
    await tester.pump();
    expect(
      find.text('We could not finish connecting. You can try again.'),
      findsOneWidget,
    );
    flow.stage = HostedFlowStage.idle;
    flow.problem = null;
    flow.notifyListeners();
    await tester.pump();
    expect(
      tester
          .widget<TextField>(find.byKey(const Key('community-link-input')))
          .enabled,
      isTrue,
    );

    await tester.pump(const Duration(milliseconds: 300));
    await tester.ensureVisible(
      find.descendant(
        of: find.byKey(const ValueKey('welcome-panel-3')),
        matching: find.text('Back'),
      ),
    );
    await tester.pump();
    await tester.tap(find.text('Back').hitTestable());
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 250));
    expect(find.text('Enter a community link').hitTestable(), findsOneWidget);
    expect(tester.takeException(), isNull);
  });
}
