import 'package:buzz/shared/widgets/compose_morph_icon.dart';
import 'package:buzz/shared/theme/buzz_icons.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  testWidgets('morph reverses in place and settles at each Tabler icon', (
    tester,
  ) async {
    Widget app(bool compose, {bool reduced = false}) => MaterialApp(
      home: MediaQuery(
        data: MediaQueryData(disableAnimations: reduced),
        child: Center(
          child: ComposeMorphIcon(compose: compose, color: Colors.black),
        ),
      ),
    );
    await tester.pumpWidget(app(false));
    expect(find.byIcon(BuzzIcons.plus), findsOneWidget);
    await tester.pumpWidget(app(true));
    await tester.pump(const Duration(milliseconds: 100));
    expect(find.byIcon(BuzzIcons.edit), findsNothing);
    await tester.pumpWidget(app(false));
    await tester.pumpAndSettle();
    expect(find.byIcon(BuzzIcons.plus), findsOneWidget);
    await tester.pumpWidget(app(true));
    await tester.pumpAndSettle();
    expect(find.byIcon(BuzzIcons.edit), findsOneWidget);
    await tester.pumpWidget(app(false, reduced: true));
    await tester.pump();
    expect(find.byIcon(BuzzIcons.plus), findsOneWidget);
    expect(tester.takeException(), isNull);
  });
}
