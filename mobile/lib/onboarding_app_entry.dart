import 'dart:async';
import 'package:flutter/material.dart';
import 'package:flutter_hooks/flutter_hooks.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';

import 'features/pairing/pairing_page.dart';
import 'features/pairing/pairing_provider.dart';
import 'hosted_app_gate.dart';
import 'shared/hosted/hosted_flow.dart';

/// Navigation over the existing branded welcome surface.
class OnboardingAppEntry extends HookConsumerWidget {
  const OnboardingAppEntry({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final existingUser = useState(false);
    final enteringCommunityLink = useState(false);
    final flow = ref.watch(hostedFlowProvider);
    useListenable(flow);
    useEffect(() {
      Future<void> restore() async {
        await flow.initialize();
        await flow.signInAccount(openBrowser: false);
      }

      unawaited(restore());
      return null;
    }, [flow]);
    final busy =
        flow.accountOnly &&
        (flow.stage == HostedFlowStage.working ||
            flow.stage == HostedFlowStage.signIn);
    final connected =
        flow.accountOnly && flow.stage == HostedFlowStage.complete;
    final problem = flow.accountOnly ? flow.problem : null;
    final status = connected
        ? 'Connected to Builderlab. Your session is saved securely on this device.'
        : problem != null
        ? switch (problem) {
            HostedFlowProblem.differentAccount =>
              'A different account is already saved on this device. Sign in with that account.',
            HostedFlowProblem.identityUnreadable =>
              'Unable to read or save your secure session. Your existing data has been kept.',
            _ => 'Could not finish signing in. Please try again.',
          }
        : null;
    void back() {
      ref.read(hostedInlineEntryProvider.notifier).setActive(false);
      unawaited(flow.leave());
      existingUser.value = false;
      enteringCommunityLink.value = false;
      FocusManager.instance.primaryFocus?.unfocus();
      ref.read(pairingProvider.notifier).reset();
    }

    return PopScope(
      canPop: !existingUser.value && !enteringCommunityLink.value,
      onPopInvokedWithResult: (didPop, _) {
        if (!didPop) back();
      },
      child: PairingPage(
        connectionLanding: true,
        existingUser: existingUser.value,
        enteringCommunityLink: enteringCommunityLink.value,
        onCommunityLink: () => enteringCommunityLink.value = true,
        onBuilderlab: busy || connected
            ? null
            : () => unawaited(flow.signInAccount()),
        accountStatus: status,
        builderlabLabel: connected
            ? 'Connected to Builderlab'
            : busy
            ? 'Signing in…'
            : 'Connect to Builderlab',
        onWelcomeBack: back,
        onAlreadyUseBuzz: () => existingUser.value = true,
      ),
    );
  }
}
