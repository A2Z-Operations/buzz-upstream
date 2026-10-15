import 'package:buzz/features/onboarding/onboarding_entry.dart';
import 'package:buzz/shared/theme/app_theme.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';

void main() {
  for (final platform in [TargetPlatform.iOS, TargetPlatform.android]) {
    testWidgets(
      'minimal entry choices on $platform dispatch only explicit intent',
      (tester) async {
        final intents = <OnboardingIntent>[];
        await tester.pumpWidget(
          ProviderScope(
            child: MaterialApp(
              theme: AppTheme.light().copyWith(platform: platform),
              home: OnboardingEntry(onIntent: intents.add),
            ),
          ),
        );
        expect(find.text('Get started'), findsOneWidget);
        expect(find.text('Already use Buzz?'), findsOneWidget);
        await tester.tap(find.text('Get started'));
        await tester.pumpAndSettle();
        expect(intents, isEmpty);
        await tester.tap(find.text('Join a community'));
        expect(intents, [OnboardingIntent.join]);
        await tester.tap(find.text('Create a community'));
        expect(intents.last, OnboardingIntent.create);
        await tester.tap(find.byType(BackButton));
        await tester.pumpAndSettle();
        await tester.tap(find.text('Already use Buzz?'));
        await tester.pumpAndSettle();
        for (final entry in {
          'Pair with desktop': OnboardingIntent.pair,
          'Sign in': OnboardingIntent.signIn,
          'Use a Nostr key': OnboardingIntent.importKey,
        }.entries) {
          await tester.tap(find.text(entry.key));
          expect(intents.last, entry.value);
        }
        expect(tester.takeException(), isNull);
      },
    );
  }

  testWidgets('large text remains scrollable on a small screen', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(320, 568);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    await tester.pumpWidget(
      ProviderScope(
        child: MaterialApp(
          builder: (context, child) => MediaQuery(
            data: MediaQuery.of(
              context,
            ).copyWith(textScaler: TextScaler.linear(2)),
            child: child!,
          ),
          home: OnboardingEntry(onIntent: (_) {}),
        ),
      ),
    );
    await tester.ensureVisible(find.text('Already use Buzz?'));
    await tester.tap(find.text('Already use Buzz?'));
    await tester.pumpAndSettle();
    await tester.ensureVisible(find.text('Use a Nostr key'));
    expect(find.text('Use a Nostr key').hitTestable(), findsOneWidget);
    expect(tester.takeException(), isNull);
  });
}
