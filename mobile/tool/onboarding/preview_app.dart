import 'package:buzz/features/onboarding/onboarding_controller.dart';
import 'package:buzz/features/onboarding/onboarding_entry.dart';
import 'package:buzz/features/pairing/pairing_input_page.dart';
import 'package:buzz/shared/theme/grid.dart';
import 'package:buzz/shared/theme/theme_extensions.dart';
import 'package:flutter/material.dart';
import 'package:flutter_hooks/flutter_hooks.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';
import 'preview_services.dart';

/// Debug-only composition: both features stay independent of one another.
class OnboardingPreviewApp extends HookConsumerWidget {
  const OnboardingPreviewApp({super.key});
  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final scenario = useState(PreviewScenario.newUser);
    final controller = useMemoized(
      () => OnboardingController(PreviewServices(scenario.value)),
      [scenario.value],
    );
    useEffect(() => controller.dispose, [controller]);
    useListenable(controller);
    final input = useTextEditingController();
    useEffect(() {
      input.clear();
      return null;
    }, [controller.stage]);

    Widget button(String label, VoidCallback action) => Padding(
      padding: const EdgeInsets.only(top: Grid.xs),
      child: FilledButton(
        onPressed: controller.operationPending ? null : action,
        child: Text(label),
      ),
    );
    final stage = controller.stage;
    Widget content;
    if (stage == OnboardingStage.welcome) {
      content = OnboardingEntry(onIntent: controller.choose);
    } else if (stage == OnboardingStage.pairing) {
      content = PairingInputPage(
        onCode: controller.pair,
        onBack: controller.leave,
      );
    } else {
      final title = switch (stage) {
        OnboardingStage.target =>
          controller.creating ? 'Create your community' : 'Find your community',
        OnboardingStage.login => 'Welcome to your community',
        OnboardingStage.importKey => 'Bring your identity',
        OnboardingStage.busy => 'Getting things ready',
        OnboardingStage.problem => 'Let’s get you connected',
        OnboardingStage.complete => 'You’re ready',
        _ => '',
      };
      content = Scaffold(
        appBar: AppBar(
          leading: BackButton(onPressed: controller.leave),
          title: const Text('Buzz'),
        ),
        body: SafeArea(
          child: Center(
            child: SingleChildScrollView(
              padding: const EdgeInsets.all(Grid.sm),
              child: ConstrainedBox(
                constraints: const BoxConstraints(maxWidth: 440),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.stretch,
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    Text(title, style: context.textTheme.headlineMedium),
                    const SizedBox(height: Grid.sm),
                    if (stage == OnboardingStage.target) ...[
                      const Text(
                        'Enter the address of the community you want to join or create.',
                      ),
                      TextField(
                        controller: input,
                        keyboardType: TextInputType.url,
                        autocorrect: false,
                        decoration: InputDecoration(
                          labelText: 'Community address',
                          hintText: 'example.communities.buzz.xyz',
                          errorText: controller.message,
                        ),
                        onSubmitted: controller.setTarget,
                      ),
                      button(
                        'Continue',
                        () => controller.setTarget(input.text),
                      ),
                    ],
                    if (stage == OnboardingStage.login) ...[
                      Text(controller.target!.origin.host),
                      const SizedBox(height: Grid.xs),
                      const Text(
                        'Sign in or create your account to continue. We’ll keep your existing Buzz identity.',
                      ),
                      button('Continue with Builderlab', controller.login),
                    ],
                    if (stage == OnboardingStage.importKey) ...[
                      const Text(
                        'Preview: enter demo-key. Do not enter a real private key.',
                      ),
                      TextField(
                        controller: input,
                        obscureText: true,
                        autocorrect: false,
                        enableSuggestions: false,
                        decoration: const InputDecoration(
                          labelText: 'Demo identity',
                        ),
                        onSubmitted: controller.importIdentity,
                      ),
                      button(
                        'Use this identity',
                        () => controller.importIdentity(input.text),
                      ),
                    ],
                    if (stage == OnboardingStage.busy) ...[
                      const Center(child: CircularProgressIndicator()),
                      const SizedBox(height: Grid.sm),
                      Semantics(
                        liveRegion: true,
                        child: Text(
                          controller.progress,
                          textAlign: TextAlign.center,
                        ),
                      ),
                    ],
                    if (stage == OnboardingStage.problem) ...[
                      Semantics(
                        liveRegion: true,
                        child: Text(controller.message!),
                      ),
                      if (controller.canRetry)
                        button('Try again', controller.retry),
                    ],
                    if (stage == OnboardingStage.complete) ...[
                      Text(
                        'Setup for ${controller.target!.origin.host} completed with simulated services.',
                      ),
                      button('Try another flow', controller.leave),
                    ],
                    if (stage != OnboardingStage.complete)
                      TextButton(
                        onPressed: controller.leave,
                        child: const Text('Leave setup'),
                      ),
                  ],
                ),
              ),
            ),
          ),
        ),
      );
    }
    return PopScope(
      canPop: stage == OnboardingStage.welcome,
      onPopInvokedWithResult: (didPop, _) {
        if (!didPop && stage != OnboardingStage.pairing) controller.leave();
      },
      child: Column(
        children: [
          Material(
            color: context.colors.surfaceContainerHighest,
            child: SafeArea(
              bottom: false,
              child: Padding(
                padding: const EdgeInsets.symmetric(horizontal: Grid.xs),
                child: Row(
                  children: [
                    const Expanded(child: Text('PREVIEW · simulated services')),
                    DropdownButton<PreviewScenario>(
                      value: scenario.value,
                      onChanged: controller.operationPending
                          ? null
                          : (value) {
                              if (value != null) scenario.value = value;
                            },
                      items: [
                        for (final value in PreviewScenario.values)
                          DropdownMenuItem(
                            value: value,
                            child: Text(value.name),
                          ),
                      ],
                    ),
                  ],
                ),
              ),
            ),
          ),
          if (stage == OnboardingStage.pairing)
            const Material(
              child: Padding(
                padding: EdgeInsets.all(Grid.xxs),
                child: Text(
                  'Preview codes: demo-hosted or demo-self-hosted. Real transfer is disabled.',
                ),
              ),
            ),
          Expanded(child: content),
        ],
      ),
    );
  }
}
