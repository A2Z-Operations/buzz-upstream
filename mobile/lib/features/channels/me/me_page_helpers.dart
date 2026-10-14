part of 'me_page.dart';

class _MeRetry extends StatelessWidget {
  final VoidCallback onRetry;
  const _MeRetry({required this.onRetry});
  @override
  Widget build(BuildContext context) => Center(
    child: Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        const Text('Couldn’t load your chats and projects.'),
        TextButton(onPressed: onRetry, child: const Text('Try again')),
      ],
    ),
  );
}

Future<String?> _name(BuildContext context, String title) async {
  if (defaultTargetPlatform == TargetPlatform.iOS) {
    final theme = utilitySurfaceThemeData(Theme.of(context));
    try {
      return await const MethodChannel(
        'buzz/profile_text_editor',
      ).invokeMethod<String>('presentProject', {
        'title': title,
        'initialValue': '',
        'placeholder': 'Project name',
        'multiline': false,
        'brightness': theme.brightness.name,
        'pageBackgroundArgb': theme.colorScheme.surface.toARGB32(),
        'containerBackgroundArgb': theme.colorScheme.surfaceContainerHighest
            .toARGB32(),
        'containerCornerRadius': Radii.container,
      });
    } on MissingPluginException {
      // Use the shared sheet when the native editor is unavailable.
    }
  }
  if (!context.mounted) return null;
  return showBuzzModalBottomSheet<String>(
    context: context,
    title: title,
    isScrollControlled: true,
    requestFocus: true,
    builder: (_) => const _NewMeProject(),
  );
}

class _NewMeProject extends HookConsumerWidget {
  const _NewMeProject();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final controller = useTextEditingController();
    useListenable(controller);
    final name = controller.text.trim();
    return Padding(
      padding: EdgeInsets.fromLTRB(
        Grid.gutter,
        Grid.md,
        Grid.gutter,
        MediaQuery.viewInsetsOf(context).bottom + Grid.gutter,
      ),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          TextField(
            controller: controller,
            autofocus: true,
            maxLength: 120,
            decoration: const InputDecoration(labelText: 'Project name'),
          ),
          const SizedBox(height: Grid.md),
          FilledButton(
            onPressed: name.isEmpty ? null : () => Navigator.pop(context, name),
            child: const Text('Create project'),
          ),
        ],
      ),
    );
  }
}

class _MeChatRow extends StatelessWidget {
  const _MeChatRow({required this.name, required this.onTap});
  final String name;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final color = navigationPrimaryForeground(context).withValues(alpha: 0.8);
    return InkWell(
      borderRadius: BorderRadius.circular(Radii.md),
      onTap: onTap,
      child: Padding(
        padding: const EdgeInsets.symmetric(
          horizontal: Grid.xxs,
          vertical: Grid.twelve,
        ),
        child: Row(
          children: [
            SizedBox(
              width: 22,
              child: Align(
                alignment: Alignment.centerLeft,
                child: Icon(BuzzIcons.messageCircle, size: 18, color: color),
              ),
            ),
            const SizedBox(width: Grid.xxs),
            Expanded(
              child: Text(
                name,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: contentListTitleTextStyle.copyWith(
                  color: color,
                  fontSize: 17,
                  height: 22 / 17,
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }
}
