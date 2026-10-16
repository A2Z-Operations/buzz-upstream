import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_hooks/flutter_hooks.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';

import 'features/channels/deep_link_dispatcher.dart';
import 'features/onboarding/hosted_connect_page.dart';
import 'features/onboarding/self_hosted_entry_sheet.dart';
import 'shared/auth/auth.dart';
import 'shared/theme/grid.dart';
import 'shared/deeplink/deep_link.dart';
import 'shared/hosted/account_identity_store.dart';
import 'shared/hosted/browser_auth.dart';
import 'shared/hosted/builderlab_account.dart';
import 'shared/hosted/hosted_admission.dart';
import 'shared/hosted/hosted_community_connection.dart';
import 'shared/hosted/hosted_flow.dart';
import 'shared/hosted/hosted_identity_service.dart';
import 'shared/hosted/device_identity_source.dart';
import 'shared/hosted/hosted_environment.dart';

// Set by a community-entry surface while it owns setup presentation.
final hostedInlineEntryProvider = NotifierProvider<HostedInlineEntry, bool>(
  HostedInlineEntry.new,
);

class HostedInlineEntry extends Notifier<bool> {
  @override
  bool build() => false;

  void setActive(bool active) {
    if (ref.mounted) state = active;
  }
}

/// Signals a completed local admission to the currently mounted Add route.
final communityEntryCompletionProvider =
    NotifierProvider<CommunityEntryCompletion, int>(
      CommunityEntryCompletion.new,
    );

class CommunityEntryCompletion extends Notifier<int> {
  @override
  int build() => 0;

  void complete() {
    if (ref.mounted) state++;
  }
}

final hostedFlowProvider = Provider<HostedFlow>((ref) {
  final account = BuilderlabAccountClient();
  final browser = BrowserAuth();
  final store = AccountIdentityStore();
  final admission = HostedAdmission(account: account, store: store);
  late final HostedFlow flow;
  flow = HostedFlow(
    account: account,
    browser: browser,
    store: store,
    identity: HostedIdentityService(
      account: account,
      store: store,
      deviceIdentities: DeviceIdentitySource(),
    ),
    enterCommunity: (link, subject, pubkey, isCurrent) async {
      await connectHostedCommunity(
        link: link,
        subject: subject,
        pubkey: pubkey,
        isCurrent: isCurrent,
        storage: ref.read(communityStorageProvider),
        admission: admission,
        persist: (community, current) => ref
            .read(authProvider.notifier)
            .authenticateWithCommunity(
              flow.pairedSensitiveActionPolicy == null
                  ? community
                  : community.copyWith(
                      sensitiveActionPolicy: flow.pairedSensitiveActionPolicy,
                    ),
              shouldContinue: current,
              activate: flow.addedCommunityCount == 0,
            ),
      );
    },
  );
  ref.onDispose(() {
    flow.dispose();
    browser.dispose();
    account.dispose();
    admission.dispose();
  });
  return flow;
});

/// Composition seam shared by signed-in and signed-out application states.
class HostedAppGate extends HookConsumerWidget {
  const HostedAppGate({
    super.key,
    required this.child,
    required this.authenticated,
  });
  final Widget child;
  final bool authenticated;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final flow = ref.watch(hostedFlowProvider);
    useListenable(flow);
    useEffect(() {
      void onFlowChanged() {
        if (flow.stage == HostedFlowStage.complete) {
          ref.read(hostedInlineEntryProvider.notifier).setActive(false);
        }
      }

      flow.addListener(onFlowChanged);
      unawaited(flow.initialize());
      return () => flow.removeListener(onFlowChanged);
    }, [flow]);
    final inlineEntry = ref.watch(hostedInlineEntryProvider);
    final visible =
        flow.stage != HostedFlowStage.idle &&
        flow.stage != HostedFlowStage.complete;
    // Native browser authentication presents its own alert over the current
    // screen. Keep that screen in place while checking the session/opening it.
    final openingSignIn =
        flow.stage == HostedFlowStage.signIn ||
        (flow.accountOnly &&
            flow.stage == HostedFlowStage.working &&
            flow.account.account == null);
    final inlineAccountStatus =
        (!authenticated || inlineEntry) &&
        flow.accountOnly &&
        (flow.stage == HostedFlowStage.working ||
            (flow.stage == HostedFlowStage.blocked &&
                !flow.canChangeCommunities));
    return DeepLinkDispatcher(
      dispatchMessageLinks: authenticated && !visible,
      onboardingHandler: (routeContext, link) async {
        final relay = switch (link) {
          InviteDeepLink(:final relayUrl) => relayUrl,
          AddCommunityDeepLink(:final relayUrl) => relayUrl,
          _ => null,
        };
        if (relay != null) {
          final communities = await ref
              .read(communityStorageProvider)
              .loadAll();
          final existing = communities
              .where(
                (item) => relayIdentity(item.relayUrl) == relayIdentity(relay),
              )
              .firstOrNull;
          if (existing != null) {
            await ref
                .read(communityListProvider.notifier)
                .switchCommunity(existing.id);
            if (context.mounted) {
              ref.read(communityEntryCompletionProvider.notifier).complete();
            }
            return true;
          }
          final host = Uri.parse(relay).host.toLowerCase();
          final reserved = HostedEnvironment.isReservedHost(host);
          if (!reserved && link is AddCommunityDeepLink) {
            if (routeContext.mounted) {
              final joined = await showSelfHostedEntry(
                routeContext,
                relay: link.relayUrl,
                name: link.name,
              );
              if (joined && context.mounted) {
                ref.read(communityEntryCompletionProvider.notifier).complete();
              }
            }
            return true;
          }
          if (reserved && !HostedEnvironment.supportsRelay(Uri.parse(relay))) {
            if (routeContext.mounted) {
              await showDialog<void>(
                context: routeContext,
                builder: (context) => AlertDialog(
                  title: const Text('Community setup unavailable'),
                  content: const Text(
                    'This community needs a different or unsupported Builderlab environment. Use an app build configured for this community.',
                  ),
                  actions: [
                    TextButton(
                      onPressed: () => Navigator.pop(context),
                      child: const Text('Close'),
                    ),
                  ],
                ),
              );
            }
            return true;
          }
        }
        if (link is InviteDeepLink) {
          final host = Uri.parse(link.relayUrl).host;
          // Reserved hosted domains must never fall into legacy key generation.
          final reserved = HostedEnvironment.isReservedHost(host);
          if (!reserved) return false;
        }
        await flow.start(link);
        return true;
      },
      child: Stack(
        fit: StackFit.expand,
        children: [
          child,
          if (flow.stage == HostedFlowStage.signIn &&
              authenticated &&
              !inlineEntry)
            Positioned(
              left: Grid.sm,
              right: Grid.sm,
              bottom: 0,
              child: SafeArea(
                child: Material(
                  color: Colors.transparent,
                  child: Wrap(
                    alignment: WrapAlignment.center,
                    children: [
                      TextButton(
                        onPressed: flow.retry,
                        child: const Text('Start login again'),
                      ),
                      TextButton(
                        onPressed: flow.leave,
                        child: const Text('Leave setup'),
                      ),
                    ],
                  ),
                ),
              ),
            ),
          if (visible &&
              !openingSignIn &&
              !inlineAccountStatus &&
              (!inlineEntry ||
                  flow.stage == HostedFlowStage.chooseIdentity ||
                  flow.stage == HostedFlowStage.chooseCommunities ||
                  flow.stage == HostedFlowStage.communityConflict ||
                  flow.canChangeCommunities))
            HostedConnectPage(
              flow: flow,
              onClose: () => unawaited(flow.leave()),
            ),
        ],
      ),
    );
  }
}
