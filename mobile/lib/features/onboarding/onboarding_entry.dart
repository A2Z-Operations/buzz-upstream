import 'package:flutter/material.dart';
import 'package:flutter_hooks/flutter_hooks.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';

import '../../shared/theme/app_theme.dart';
import '../../shared/theme/grid.dart';
import '../../shared/theme/theme_extensions.dart';

/// Intent is independent of auth/key creation. Choosing one never creates a key.
enum OnboardingIntent { join, create, pair, signIn, importKey }

/// Minimal entry choices shared by the preview and future app coordinator.
/// The caller owns target/invite preservation and the actual operations.
class OnboardingEntry extends HookConsumerWidget {
  const OnboardingEntry({super.key, required this.onIntent});
  final ValueChanged<OnboardingIntent> onIntent;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final returning = useState<bool?>(null);
    return PopScope(
      canPop: returning.value == null,
      onPopInvokedWithResult: (didPop, _) {
        if (!didPop) returning.value = null;
      },
      child: Scaffold(
        appBar: AppBar(
          leading: returning.value == null
              ? null
              : BackButton(onPressed: () => returning.value = null),
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
                    Text(
                      returning.value == null
                          ? 'Your people. Your place.'
                          : returning.value!
                          ? 'Welcome back'
                          : 'Make yourself at home',
                      style: context.textTheme.headlineMedium,
                    ),
                    const SizedBox(height: Grid.sm),
                    if (returning.value == null) ...[
                      FilledButton(
                        onPressed: () => returning.value = false,
                        child: const Text('Get started'),
                      ),
                      TextButton(
                        onPressed: () => returning.value = true,
                        child: const Text('Already use Buzz?'),
                      ),
                    ] else ...[
                      for (final option
                          in (returning.value!
                                  ? const {
                                      OnboardingIntent.pair:
                                          'Pair with desktop',
                                      OnboardingIntent.signIn: 'Sign in',
                                      OnboardingIntent.importKey:
                                          'Use a Nostr key',
                                    }
                                  : const {
                                      OnboardingIntent.join: 'Join a community',
                                      OnboardingIntent.create:
                                          'Create a community',
                                    })
                              .entries)
                        Padding(
                          padding: const EdgeInsets.only(bottom: Grid.xxs),
                          child: OutlinedButton(
                            style: OutlinedButton.styleFrom(
                              shape: RoundedRectangleBorder(
                                borderRadius: BorderRadius.circular(Radii.md),
                              ),
                            ),
                            onPressed: () => onIntent(option.key),
                            child: Text(option.value),
                          ),
                        ),
                    ],
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
