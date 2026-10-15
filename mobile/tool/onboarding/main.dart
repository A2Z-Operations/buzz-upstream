import 'package:buzz/shared/theme/app_theme.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';
import 'preview_app.dart';

/// Isolated debug composition. No production services or credentials.
void main() {
  if (!kDebugMode) throw StateError('Onboarding preview requires debug mode');
  runApp(
    ProviderScope(
      child: MaterialApp(
        theme: AppTheme.light(),
        darkTheme: AppTheme.dark(),
        home: const OnboardingPreviewApp(),
      ),
    ),
  );
}
