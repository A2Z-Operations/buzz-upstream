part of 'me_page.dart';

Future<(Channel, String)?> _showNewMeChat(
  BuildContext context,
  WidgetRef ref,
) async {
  if (defaultTargetPlatform == TargetPlatform.iOS) {
    final sync = ref.read(meSyncProvider);
    final id = const Uuid().v4();
    String name = '';
    String? assistant;
    String? error;
    var retry = false;
    while (context.mounted) {
      sync.check();
      final directory = await ref.read(meAssistantsProvider.future);
      if (!context.mounted) return null;
      sync.check();
      if (!context.mounted) return null;
      final theme = utilitySurfaceThemeData(Theme.of(context));
      Map<Object?, Object?>? draft;
      try {
        draft = await const MethodChannel('buzz/profile_text_editor')
            .invokeMethod<Map<Object?, Object?>>('presentChat', {
              'title': 'New chat',
              'initialValue': name,
              'placeholder': 'Chat name',
              'multiline': false,
              'brightness': theme.brightness.name,
              'pageBackgroundArgb': theme.colorScheme.surface.toARGB32(),
              'containerBackgroundArgb': theme
                  .colorScheme
                  .surfaceContainerHighest
                  .toARGB32(),
              'containerCornerRadius': Radii.container,
              'selectedAssistant': assistant,
              'fieldsLocked': retry,
              'error': error,
              'assistants': [
                for (final agent in directory)
                  {
                    'id': agent.pubkey,
                    'name': agent.displayName ?? 'Agent',
                    'artworkScale':
                        usesInsetAgentArtwork(
                          ref
                              .read(userCacheProvider.notifier)
                              .get(agent.pubkey)
                              ?.avatarUrl,
                        )
                        ? '$animatedAvatarArtworkScale'
                        : '1',
                    'avatarUrl':
                        ref
                            .read(userCacheProvider.notifier)
                            .get(agent.pubkey)
                            ?.avatarUrl ??
                        '',
                  },
              ],
            });
      } on MissingPluginException {
        break;
      }
      if (draft == null || !context.mounted) return null;
      name = (draft['name'] as String? ?? '').trim();
      assistant = draft['assistant'] as String?;
      if (name.isEmpty || assistant == null) return null;
      retry = true;
      try {
        final created = await _createMeChat(ref, sync, id, name, assistant);
        return (created, assistant);
      } catch (failure) {
        error =
            '${failure.toString().replaceFirst('Bad state: ', '')}\nRetry continues this same chat.';
      }
    }
  }
  if (!context.mounted) return null;
  return showBuzzModalBottomSheet<(Channel, String)>(
    context: context,
    title: 'New chat',
    isScrollControlled: true,
    requestFocus: true,
    builder: (_) => const _NewMeChat(),
  );
}

Future<Channel> _createMeChat(
  WidgetRef ref,
  MeSync sync,
  String channelId,
  String name,
  String assistant,
) async {
  final actions = ref.read(channelActionsProvider);
  final owned = await isOwnedMeAssistant(sync, assistant);
  sync.check();
  if (!owned) {
    throw StateError('This agent is no longer available.');
  }
  final head = await sync.head();
  (head == null ? const MeGroups() : sync.decode(head))
      .place(channelId)
      .encode(sync.community);
  final all = await ref.read(channelsProvider.future);
  sync.check();
  final existing = all.where((c) => c.id == channelId).firstOrNull;
  final channel =
      existing ??
      await actions.createChannel(
        channelId: channelId,
        name: name,
        channelType: 'stream',
        visibility: 'private',
      );
  sync.check();
  final members = await ref.read(channelMembersProvider(channelId).future);
  if (!members.any((m) => m.pubkey == assistant)) {
    await actions.addMembers(channelId: channelId, pubkeys: [assistant]);
  }
  sync.check();
  await sync.update((current) => current.place(channelId));
  return channel;
}
