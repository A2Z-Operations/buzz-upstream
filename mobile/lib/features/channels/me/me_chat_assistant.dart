import 'package:flutter/material.dart';
import 'package:flutter_hooks/flutter_hooks.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';

import '../../../shared/me/me_assistants.dart';
import '../../../shared/me/me_sync.dart';
import '../../../shared/profile/user_cache_provider.dart';
import '../../../shared/relay/relay.dart';
import '../../../shared/theme/theme.dart';
import '../../../shared/theme/buzz_icons.dart';
import '../../../shared/widgets/avatar_image.dart';
import '../../../shared/widgets/frosted_app_bar.dart';
import '../../../shared/widgets/modal_presentation.dart';
import '../channel_management_provider.dart';
import 'me_assistant_picker_sheet.dart';

/// Resolves a remembered choice or the assistant already used in a synced chat.
/// Ambiguous chats require a choice instead of silently picking another agent.
String? resolveMeAssistant({
  required String? selected,
  required Set<String> available,
  required String viewer,
  required List<NostrEvent> messages,
}) {
  if (selected != null) return available.contains(selected) ? selected : null;
  if (available.length == 1) return available.single;
  final sent =
      messages
          .where(
            (event) =>
                event.pubkey == viewer &&
                EventKind.channelMessageEventKinds.contains(event.kind),
          )
          .toList()
        ..sort((a, b) => b.createdAt.compareTo(a.createdAt));
  for (final event in sent) {
    final recipients = event.tags
        .where((tag) => tag.length > 1 && tag[0] == 'p')
        .map((tag) => tag[1])
        .where(available.contains)
        .toSet();
    if (recipients.length == 1) return recipients.single;
    if (recipients.length > 1) return null;
  }
  return null;
}

/// Header avatar and persistent mobile selection for a personal chat.
({String? selected, IosNavigationAction nativeAction, Widget action})
useMeChatAssistant(
  BuildContext context,
  WidgetRef ref, {
  required bool enabled,
  required String channelId,
  required String? initial,
  required List<NostrEvent> messages,
}) {
  final sync = enabled ? ref.watch(meSyncProvider) : null;
  final key = sync == null
      ? ''
      : 'me-assistant:${sync.community}:${sync.pubkey}:$channelId';
  final prefs = sync?.prefs;
  final selection = useState((key, initial ?? prefs?.getString(key)));
  final busy = useState(false);
  final agents = enabled ? ref.watch(meAssistantsProvider).value ?? [] : [];
  final members = enabled
      ? ref.watch(channelMembersProvider(channelId)).value ?? []
      : [];
  final selected = resolveMeAssistant(
    selected: selection.value.$1 == key
        ? selection.value.$2
        : prefs?.getString(key),
    available: {
      for (final agent in agents)
        if (members.any((member) => member.pubkey == agent.pubkey))
          agent.pubkey,
    },
    viewer: sync?.pubkey ?? '',
    messages: messages,
  );
  final profile = enabled && selected != null
      ? ref.watch(userCacheProvider)[selected] ??
            ref.read(userCacheProvider.notifier).get(selected)
      : null;
  final name =
      profile?.displayName ??
      agents
          .where((agent) => agent.pubkey == selected)
          .firstOrNull
          ?.displayName;
  final label = selected == null
      ? 'Choose an agent'
      : 'Change agent: ${name ?? 'Agent'}';
  Future<void> choose() async {
    if (busy.value || sync == null) return;
    busy.value = true;
    try {
      sync.check();
      final picked = await showBuzzModalBottomSheet<String>(
        context: context,
        title: 'Agent',
        isScrollControlled: true,
        showDragHandle: true,
        builder: (_) => MeAssistantPickerSheet(selected: selected),
      );
      if (picked == null || !context.mounted) return;
      sync.check();
      ref.invalidate(meAssistantsProvider);
      final current = await ref.read(meAssistantsProvider.future);
      sync.check();
      if (!current.any((agent) => agent.pubkey == picked)) {
        throw StateError('This agent is no longer available.');
      }
      final members = await ref.read(channelMembersProvider(channelId).future);
      sync.check();
      if (!members.any((member) => member.pubkey == picked)) {
        await ref
            .read(channelActionsProvider)
            .addMembers(channelId: channelId, pubkeys: [picked]);
        ref.invalidate(channelMembersProvider(channelId));
      }
      sync.check();
      if (prefs != null && !await prefs.setString(key, picked)) {
        throw StateError('Could not save your agent choice. Try again.');
      }
      if (context.mounted) selection.value = (key, picked);
    } catch (error) {
      if (context.mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(
            content: Text(error.toString().replaceFirst('Bad state: ', '')),
          ),
        );
      }
    } finally {
      if (context.mounted) busy.value = false;
    }
  }

  return (
    selected: selected,
    nativeAction: IosNavigationAction(
      label: label,
      symbol: 'person.crop.circle',
      avatarIdentity: selected,
      avatarIsAgent: selected != null,
      imageUrl: profile?.avatarUrl,
      avatarInitial:
          profile?.initial ?? (name?.isNotEmpty == true ? name![0] : '?'),
      onPressed: busy.value ? null : choose,
    ),
    action: IconButton(
      key: const ValueKey('me-assistant-button'),
      tooltip: label,
      onPressed: busy.value ? null : choose,
      icon: AvatarImage(
        imageUrl: profile?.avatarUrl,
        radius: 16,
        isAgent: selected != null,
        backgroundColor: context.colors.primaryContainer,
        fallback: selected == null
            ? const Icon(BuzzIcons.plus, size: 20)
            : Text(
                profile?.initial ?? (name?.isNotEmpty == true ? name![0] : '?'),
              ),
      ),
    ),
  );
}
