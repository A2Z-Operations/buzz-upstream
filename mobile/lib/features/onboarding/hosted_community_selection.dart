import 'package:flutter/material.dart';
import 'package:flutter_hooks/flutter_hooks.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';

import '../../shared/deeplink/pending_deep_link_provider.dart';
import '../../shared/hosted/hosted_flow.dart';
import '../../shared/theme/grid.dart';
import 'community_link_input.dart';

/// Public community choices only; identity material remains in secure storage.
class HostedCommunitySelection extends HookConsumerWidget {
  const HostedCommunitySelection({super.key, required this.flow});
  final HostedFlow flow;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    useListenable(flow);
    final invite = useTextEditingController();
    final error = useState<String?>(null);
    Future<void> openLink() async {
      final uri = parseCommunityLinkInput(invite.text);
      if (uri == null) {
        error.value = 'Enter a valid community address or invite link.';
        return;
      }
      final pending = ref.read(pendingDeepLinkProvider.notifier);
      await flow.leave();
      if (flow.stage == HostedFlowStage.idle) pending.open(uri);
    }

    if (flow.communities.isNotEmpty) {
      return Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          for (final community in flow.communities)
            CheckboxListTile(
              key: ValueKey(community.relayUrl),
              title: Text(community.name),
              subtitle: Text(Uri.parse(community.relayUrl).host),
              value: flow.selectedRelays.contains(community.relayUrl),
              onChanged: (value) =>
                  flow.selectCommunity(community.relayUrl, value ?? false),
            ),
          const SizedBox(height: Grid.sm),
          FilledButton(
            onPressed: flow.selectedRelays.isEmpty
                ? null
                : flow.addSelectedCommunities,
            child: const Text('Continue'),
          ),
        ],
      );
    }
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        FilledButton(
          onPressed: flow.openCommunityCreation,
          child: const Text('Create a community in Builderlab'),
        ),
        TextButton(
          onPressed: flow.refreshCommunities,
          child: const Text('Refresh communities'),
        ),
        const SizedBox(height: Grid.sm),
        TextField(
          controller: invite,
          keyboardType: TextInputType.url,
          textInputAction: TextInputAction.go,
          onSubmitted: (_) => openLink(),
          onChanged: (_) => error.value = null,
          autocorrect: false,
          enableSuggestions: false,
          decoration: InputDecoration(
            labelText: 'Community address or invite link',
            errorText: error.value,
          ),
        ),
        OutlinedButton(
          onPressed: openLink,
          child: const Text('Open community link'),
        ),
      ],
    );
  }
}
