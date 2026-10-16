import 'package:buzz/features/channels/channel.dart';
import 'package:buzz/features/channels/channels_provider.dart';
import 'package:buzz/features/onboarding/hosted_profile_setup.dart';
import 'package:buzz/hosted_app_gate.dart';
import 'package:buzz/onboarding_app_entry.dart';
import 'package:buzz/shared/auth/auth.dart';
import 'package:buzz/shared/deeplink/pending_deep_link_provider.dart';
import 'package:buzz/shared/hosted/builderlab_account.dart';
import 'package:buzz/shared/hosted/hosted_flow.dart';
import 'package:flutter/material.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';

import '../../helpers/hosted_communities_fixture.dart';
import '../../helpers/widget_helpers.dart';

class _NoChannels extends ChannelsNotifier {
  @override
  Future<List<Channel>> build() async => [];
}

void main() {
  testWidgets('dismissing profile keeps batch retry above pushed Add route', (
    tester,
  ) async {
    await tester.binding.setSurfaceSize(const Size(440, 956));
    addTearDown(() => tester.binding.setSurfaceSize(null));
    FlutterSecureStorage.setMockInitialValues({});
    PendingDeepLinkNotifier.debugUriStreamOverride = const Stream<Uri>.empty();
    final f = HostedCommunitiesFixture();
    await f.initialize(widget: true);
    addTearDown(() async {
      PendingDeepLinkNotifier.debugUriStreamOverride = null;
      await f.dispose();
    });
    f.rows = [f.row('one'), f.row('two')];
    var secondAttempts = 0;
    final profileWrites = <String>[];
    f.beforeAdmission = (name) async {
      if (name == 'one') {
        await f.flow.requestProfile((value) async => profileWrites.add(value));
      } else if (++secondAttempts == 1) {
        throw AccountFailure.unavailable;
      }
    };
    await tester.pumpWidget(
      WidgetHelpers.testable(
        disableAnimations: true,
        overrides: [
          hostedFlowProvider.overrideWithValue(f.flow),
          channelsProvider.overrideWith(_NoChannels.new),
        ],
        child: HostedAppGate(
          authenticated: true,
          child: Builder(
            builder: (context) => Center(
              child: FilledButton(
                onPressed: () => Navigator.of(context).push(
                  MaterialPageRoute<void>(
                    builder: (_) =>
                        const OnboardingAppEntry(addingCommunity: true),
                  ),
                ),
                child: const Text('Add community'),
              ),
            ),
          ),
        ),
      ),
    );
    await tester.pumpAndSettle();
    final container = ProviderScope.containerOf(
      tester.element(find.byType(HostedAppGate)),
    );
    await container.read(authProvider.future);
    await tester.tap(find.text('Add community').hitTestable());
    await tester.pumpAndSettle();
    await tester.tap(find.text('Continue with Builderlab').hitTestable());
    await tester.pumpAndSettle();
    expect(find.byType(CheckboxListTile).hitTestable(), findsNWidgets(2));
    await tester.tap(find.text('Continue').hitTestable());
    await tester.pumpAndSettle();
    expect(f.flow.stage, HostedFlowStage.profile);
    expect(find.byType(HostedProfileSetup), findsOneWidget);
    expect(find.byType(TextField).hitTestable(), findsOneWidget);
    expect(secondAttempts, 0);

    // Exercise actual route dismissal, not the explicit Skip button.
    Navigator.of(
      tester.element(find.byType(HostedProfileSetup)),
      rootNavigator: true,
    ).pop();
    await tester.pumpAndSettle();
    expect(profileWrites, isEmpty);
    expect(f.admitted, ['one']);
    expect(secondAttempts, 1);
    expect(f.flow.stage, HostedFlowStage.blocked);
    expect(
      find
          .text('We could not finish connecting. You can try again.')
          .hitTestable(),
      findsOneWidget,
    );
    expect(find.text('Try again').hitTestable(), findsOneWidget);
    await tester.tap(find.text('Try again').hitTestable());
    await tester.pumpAndSettle();
    expect(secondAttempts, 2);
    expect(f.admitted, ['one', 'two']);
    expect(f.flow.stage, HostedFlowStage.complete);
    expect(find.byType(OnboardingAppEntry), findsNothing);
    expect(find.text('Add community').hitTestable(), findsOneWidget);
    expect(tester.takeException(), isNull);
  });
}
