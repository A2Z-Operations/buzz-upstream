import 'package:buzz/features/onboarding/hosted_connect_sheet.dart';
import 'package:buzz/shared/hosted/hosted_flow.dart';
import 'package:buzz/shared/hosted/hosted_join_policy.dart';
import 'package:flutter/material.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';
import '../../helpers/hosted_communities_fixture.dart';
import '../../helpers/widget_helpers.dart';

void main() {
  testWidgets('policy cannot advance before age consent; leaving cancels it', (
    tester,
  ) async {
    FlutterSecureStorage.setMockInitialValues({});
    final f = HostedCommunitiesFixture();
    await f.initialize(widget: true);
    addTearDown(f.dispose);
    final accepted = f.flow.confirmJoinPolicy(
      const HostedJoinPolicy(
        version: 'v1',
        ageRequired: true,
        terms: 'Community terms',
        privacy: 'Privacy text',
      ),
    );
    await tester.pumpWidget(
      WidgetHelpers.testable(
        child: Scaffold(body: HostedConnectSheet(flow: f.flow)),
      ),
    );
    await tester.pumpAndSettle();
    final button = find.widgetWithText(FilledButton, 'Accept and join');
    expect(tester.widget<FilledButton>(button).onPressed, isNull);
    expect(find.text('Try again'), findsNothing);
    await tester.tap(find.text('Terms of service'));
    await tester.pumpAndSettle();
    expect(find.text('Community terms'), findsOneWidget);
    await tester.tap(find.byType(CheckboxListTile));
    await tester.pumpAndSettle();
    await tester.tap(button);
    expect(await accepted, isTrue);
    expect(f.flow.stage, HostedFlowStage.working);
    final abandoned = f.flow.confirmJoinPolicy(
      const HostedJoinPolicy(version: 'v2', ageRequired: false),
    );
    await f.flow.leave();
    expect(await abandoned, isFalse);
    expect(f.flow.stage, HostedFlowStage.idle);
    f.flow.acceptJoinPolicy();
    expect(f.flow.stage, HostedFlowStage.idle);
  });
}
