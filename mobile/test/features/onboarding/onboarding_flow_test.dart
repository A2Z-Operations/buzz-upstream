import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';
import '../../../tool/onboarding/preview_app.dart';

void main() {
  testWidgets('hosted join traverses actual screens without extra Next steps', (
    tester,
  ) async {
    await tester.pumpWidget(
      const ProviderScope(child: MaterialApp(home: OnboardingPreviewApp())),
    );
    await tester.tap(find.text('Get started'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Join a community'));
    await tester.pumpAndSettle();
    await tester.enterText(
      find.byType(TextField),
      'example.communities.buzz.xyz',
    );
    await tester.tap(find.text('Continue'));
    await tester.pumpAndSettle();
    expect(find.text('Continue with Builderlab'), findsOneWidget);
    await tester.tap(find.text('Continue with Builderlab'));
    await tester.pumpAndSettle();
    expect(find.text('You’re ready'), findsOneWidget);
    expect(find.textContaining('simulated services.'), findsOneWidget);
  });
  testWidgets('import and self-hosted join skip hosted login', (tester) async {
    await tester.pumpWidget(
      const ProviderScope(child: MaterialApp(home: OnboardingPreviewApp())),
    );
    await tester.tap(find.text('Already use Buzz?'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Use a Nostr key'));
    await tester.pumpAndSettle();
    await tester.enterText(find.byType(TextField), 'demo-key');
    await tester.tap(find.text('Use this identity'));
    await tester.pumpAndSettle();
    await tester.enterText(find.byType(TextField), 'community.example.org');
    await tester.tap(find.text('Continue'));
    await tester.pumpAndSettle();
    expect(find.text('You’re ready'), findsOneWidget);
    expect(find.text('Continue with Builderlab'), findsNothing);
  });
}
