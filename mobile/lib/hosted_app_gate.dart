import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_hooks/flutter_hooks.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';

import 'features/channels/deep_link_dispatcher.dart';
import 'features/onboarding/hosted_connect_page.dart';
import 'shared/auth/auth.dart';
import 'shared/deeplink/deep_link.dart';
import 'shared/hosted/account_identity_store.dart';
import 'shared/hosted/browser_auth.dart';
import 'shared/hosted/builderlab_account.dart';
import 'shared/hosted/hosted_admission.dart';
import 'shared/hosted/hosted_flow.dart';
import 'shared/hosted/hosted_identity_service.dart';

// Set by a community-entry surface while it owns setup presentation.
final hostedInlineEntryProvider = NotifierProvider<HostedInlineEntry, bool>(
  HostedInlineEntry.new,
);

class HostedInlineEntry extends Notifier<bool> {
  @override
  bool build() => false;

  void setActive(bool active) => state = active;
}

final hostedFlowProvider = Provider<HostedFlow>((ref) {
  final account = BuilderlabAccountClient();
  final browser = BrowserAuth();
  final store = AccountIdentityStore();
  final admission = HostedAdmission(account: account, store: store);
  final flow = HostedFlow(
    account: account,
    browser: browser,
    store: store,
    identity: HostedIdentityService(account: account, store: store),
    enterCommunity: (link, subject, pubkey, isCurrent) async {
      final relay = switch (link) {
        InviteDeepLink(:final relayUrl) => relayUrl,
        AddCommunityDeepLink(:final relayUrl) => relayUrl,
        _ => throw AccountFailure.invalidResponse,
      };
      final communities = await ref.read(communityStorageProvider).loadAll();
      final existing = communities
          .where(
            (item) =>
                Uri.parse(item.relayUrl).replace(path: '').toString() ==
                Uri.parse(relay).replace(path: '').toString(),
          )
          .firstOrNull;
      if (existing != null && existing.pubkey != pubkey) {
        throw AccountIdentityFailure.mismatch;
      }
      if (!isCurrent()) return;
      final prepared = await admission.prepare(
        link,
        subject,
        pubkey,
        isCurrent: isCurrent,
      );
      final community = existing == null
          ? prepared
          : existing.copyWith(nsec: prepared.nsec);
      if (!isCurrent()) return;
      await ref
          .read(authProvider.notifier)
          .authenticateWithCommunity(community, shouldContinue: isCurrent);
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
        !flow.accountOnly &&
        flow.stage != HostedFlowStage.idle &&
        flow.stage != HostedFlowStage.complete;
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
                (item) =>
                    _relayIdentity(item.relayUrl) == _relayIdentity(relay),
              )
              .firstOrNull;
          if (existing != null) {
            await ref
                .read(communityListProvider.notifier)
                .switchCommunity(existing.id);
            return true;
          }
          final host = Uri.parse(relay).host.toLowerCase();
          final reserved =
              host == 'buzz.xyz' ||
              host.endsWith('.buzz.xyz') ||
              host == 'builderlab.xyz' ||
              host.endsWith('.builderlab.xyz');
          if (!reserved && link is AddCommunityDeepLink) {
            if (routeContext.mounted) {
              await showDialog<void>(
                context: routeContext,
                builder: (context) => AlertDialog(
                  title: const Text('An invite or pairing code is needed'),
                  content: const Text(
                    'To join this community in this build, enter an invite link '
                    'or pair with your desktop. Setup using only an address '
                    'is not available for this community yet.',
                  ),
                  actions: [
                    TextButton(
                      onPressed: () => Navigator.pop(context),
                      child: const Text('Back'),
                    ),
                  ],
                ),
              );
            }
            return true;
          }
          if (reserved && !host.endsWith('.communities.buzz.xyz')) {
            if (routeContext.mounted) {
              await showDialog<void>(
                context: routeContext,
                builder: (context) => AlertDialog(
                  title: const Text('Community setup unavailable'),
                  content: const Text(
                    'This community does not support account setup in this build yet.',
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
          final reserved =
              host == 'buzz.xyz' ||
              host.endsWith('.buzz.xyz') ||
              host == 'builderlab.xyz' ||
              host.endsWith('.builderlab.xyz');
          if (!reserved) return false;
        }
        await flow.start(link);
        return true;
      },
      child: Stack(
        fit: StackFit.expand,
        children: [
          child,
          if (visible && !inlineEntry)
            HostedConnectPage(
              flow: flow,
              onClose: () => unawaited(flow.leave()),
            ),
        ],
      ),
    );
  }
}

({bool secure, String host, int port}) _relayIdentity(String value) {
  final uri = Uri.parse(value);
  final secure = uri.scheme == 'wss' || uri.scheme == 'https';
  return (
    secure: secure,
    host: uri.host.toLowerCase(),
    port: uri.hasPort ? uri.port : (secure ? 443 : 80),
  );
}
