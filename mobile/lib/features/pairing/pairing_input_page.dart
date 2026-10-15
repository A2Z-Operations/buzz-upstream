import 'package:flutter/material.dart';
import 'package:flutter_hooks/flutter_hooks.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';

import '../../shared/theme/grid.dart';
import 'pairing_qr_scanner.dart';

/// Collects a pairing code using the existing platform-specific scanner.
/// The coordinator owns validation, transfer, and community admission.
class PairingInputPage extends HookConsumerWidget {
  const PairingInputPage({super.key, required this.onCode, this.onBack});
  final ValueChanged<String> onCode;
  final VoidCallback? onBack;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final code = useTextEditingController();
    final fallbackVisible = useState(false);
    final opening = useState(false);
    final error = useState<String?>(null);

    void submit(String value) {
      final trimmed = value.trim();
      if (trimmed.isEmpty) {
        error.value = 'Scan a QR code or paste a pairing code.';
        return;
      }
      error.value = null;
      onCode(trimmed);
    }

    Future<void> scan() async {
      if (opening.value) return;
      opening.value = true;
      try {
        final portal = await usesDynamicIslandQrScannerPortal();
        if (!context.mounted) return;
        if (portal) {
          final result = await showDynamicIslandPairingQrScanner(context);
          if (context.mounted && result != null) submit(result);
        } else {
          fallbackVisible.value = true;
        }
      } finally {
        if (context.mounted) opening.value = false;
      }
    }

    final surface = Scaffold(
      appBar: AppBar(
        leading: onBack == null ? null : BackButton(onPressed: onBack),
        title: const Text('Pair with desktop'),
      ),
      body: SafeArea(
        child: ListView(
          padding: const EdgeInsets.all(Grid.sm),
          children: [
            const Text(
              'Open pairing in your desktop Buzz app, then scan its QR code.',
            ),
            const SizedBox(height: Grid.sm),
            FilledButton(
              onPressed: opening.value ? null : scan,
              child: const Text('Scan a QR code'),
            ),
            const SizedBox(height: Grid.sm),
            TextField(
              controller: code,
              autocorrect: false,
              enableSuggestions: false,
              decoration: InputDecoration(
                labelText: 'Pairing code',
                errorText: error.value,
              ),
              onSubmitted: submit,
            ),
            const SizedBox(height: Grid.xs),
            OutlinedButton(
              onPressed: () => submit(code.text),
              child: const Text('Continue'),
            ),
          ],
        ),
      ),
    );
    if (!fallbackVisible.value) {
      return PopScope(
        canPop: onBack == null,
        onPopInvokedWithResult: (didPop, _) {
          if (!didPop) onBack?.call();
        },
        child: surface,
      );
    }
    return FallbackPairingQrScanner(
      appSurface: surface,
      onClosed: (result) {
        fallbackVisible.value = false;
        if (result != null) submit(result);
      },
    );
  }
}
