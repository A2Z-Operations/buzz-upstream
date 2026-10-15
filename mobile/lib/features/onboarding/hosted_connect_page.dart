import 'package:flutter/material.dart';
import 'package:flutter_hooks/flutter_hooks.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';

import '../../shared/hosted/hosted_flow.dart';
import '../../shared/hosted/hosted_flow_problem_message.dart';
import '../../shared/utils/string_utils.dart';
import '../../shared/theme/grid.dart';
import '../../shared/theme/theme_extensions.dart';

/// Presents the app-scoped real flow without retaining credentials or keys.
class HostedConnectPage extends HookConsumerWidget {
  const HostedConnectPage({
    super.key,
    required this.flow,
    required this.onClose,
  });
  final HostedFlow flow;
  final VoidCallback onClose;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    useListenable(flow);
    final working = flow.stage == HostedFlowStage.working;
    final message = switch (flow.stage) {
      HostedFlowStage.idle => 'Continue connecting your account.',
      HostedFlowStage.working => 'Connecting your account…',
      HostedFlowStage.chooseIdentity =>
        flow.identityChoices.any((choice) => choice.fromCloud)
            ? 'Use your Builderlab identity on this device? It differs from your saved community identities. Those keys and your cloud backups will be kept. This app cannot change the selected Builderlab identity later.'
            : 'Choose the identity to connect to Builderlab. It will be bound to your account and backed up. Existing keys will be kept. This app cannot change the selected Builderlab identity later.',
      HostedFlowStage.signIn => 'Complete sign-in in your browser.',
      HostedFlowStage.browserReturned =>
        'Continue setup in your browser. Open your community in Buzz when it is ready.',
      HostedFlowStage.complete => 'Your community is ready.',
      HostedFlowStage.blocked => hostedFlowProblemMessage(flow.problem),
    };
    void close() {
      onClose();
    }

    return PopScope(
      canPop: false,
      onPopInvokedWithResult: (didPop, _) {
        if (!didPop) close();
      },
      child: Scaffold(
        appBar: AppBar(
          title: const Text('Connect to Buzz'),
          leading: BackButton(onPressed: close),
        ),
        body: SafeArea(
          child: Center(
            child: SingleChildScrollView(
              padding: const EdgeInsets.all(Grid.sm),
              child: ConstrainedBox(
                constraints: const BoxConstraints(maxWidth: 440),
                child: Column(
                  mainAxisSize: MainAxisSize.min,
                  crossAxisAlignment: CrossAxisAlignment.stretch,
                  children: [
                    Text(message, style: context.textTheme.titleLarge),
                    const SizedBox(height: Grid.sm),
                    for (final choice in flow.identityChoices)
                      Padding(
                        padding: const EdgeInsets.only(bottom: Grid.sm),
                        child: OutlinedButton(
                          onPressed: () => flow.chooseIdentity(choice.pubkey),
                          child: Padding(
                            padding: const EdgeInsets.all(Grid.sm),
                            child: Column(
                              children: [
                                Text(choice.label),
                                Text(fullNpub(choice.pubkey) ?? choice.pubkey),
                              ],
                            ),
                          ),
                        ),
                      ),
                    if (working)
                      const Center(child: CircularProgressIndicator()),
                    if (!working &&
                        flow.stage != HostedFlowStage.chooseIdentity &&
                        flow.stage != HostedFlowStage.complete &&
                        flow.stage != HostedFlowStage.browserReturned &&
                        flow.canRetry)
                      FilledButton(
                        onPressed: flow.retry,
                        child: Text(
                          flow.stage == HostedFlowStage.signIn
                              ? 'Start login again'
                              : 'Try again',
                        ),
                      ),
                    TextButton(
                      onPressed: close,
                      child: const Text('Leave setup'),
                    ),
                  ],
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}
