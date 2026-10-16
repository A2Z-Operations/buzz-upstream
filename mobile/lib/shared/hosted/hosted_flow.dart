import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:url_launcher/url_launcher.dart';

import '../deeplink/deep_link.dart';
import 'account_identity_store.dart';
import 'device_identity_source.dart';
import 'browser_auth.dart';
import 'builderlab_account.dart';
import 'hosted_identity_service.dart';
import 'hosted_backup_service.dart';
import 'hpke_backup.dart';
import 'hosted_environment.dart';
import 'hosted_communities.dart';
import 'hosted_community_connection.dart';
import 'web_connect_binding.dart';

enum HostedFlowStage {
  idle,
  working,
  signIn,
  chooseIdentity,
  chooseCommunities,
  communityConflict,
  blocked,
  browserReturned,
  complete,
}

enum HostedFlowProblem {
  retry,
  denied,
  differentAccount,
  identityUnreadable,
  backupUnavailable,
  invalidResponse,
  requestRejected,
  identityMismatch,
  identityUnavailable,
  backupRequired,
  bindingRequired,
  unsupportedEnvironment,
}

/// Resolves only after relay admission is proven and local persistence completes.
/// Implementations must check [isCurrent] immediately before durable mutations.
typedef HostedCommunityAdmission =
    Future<void> Function(
      BuzzDeepLink target,
      String subject,
      String pubkey,
      bool Function() isCurrent,
    );

/// App-scoped owner: callback subscriptions must outlive any individual page.
/// Only public progress reaches widgets; pending auth secrets remain in BrowserAuth.
class HostedFlow extends ChangeNotifier {
  HostedFlow({
    required this.account,
    required this.browser,
    required this.store,
    required this.identity,
    required this.enterCommunity,
    this.hpke,
    Future<bool> Function(Uri)? openWeb,
  }) : _openWeb =
           openWeb ??
           ((uri) => launchUrl(uri, mode: LaunchMode.externalApplication));
  final BuilderlabAccountClient account;
  final BrowserAuth browser;
  final AccountIdentityStore store;
  final HostedIdentityService identity;
  final HostedCommunityAdmission enterCommunity;
  final HpkeBackup? hpke;
  bool get canRetry => problem == null || problem == HostedFlowProblem.retry;
  final Future<bool> Function(Uri) _openWeb;
  HostedFlowStage stage = HostedFlowStage.idle;
  HostedFlowProblem? problem;
  List<DeviceIdentityChoice> identityChoices = const [];
  BuzzDeepLink? _target;
  bool accountOnly = false;
  List<AddCommunityDeepLink> communities = const [];
  Set<String> selectedRelays = const {};
  final Set<String> _addedRelays = {};
  bool _addingCommunities = false;
  String? _batchSubject;
  String? _conflictingRelay;
  bool get canChangeCommunities =>
      accountOnly &&
      account.account?.subject == _batchSubject &&
      (stage == HostedFlowStage.blocked ||
          stage == HostedFlowStage.communityConflict) &&
      communities.isNotEmpty;
  String? addingCommunityName;
  int get addedCommunityCount => _addedRelays.length;
  Future<void> _tail = Future.value();
  StreamSubscription<BrowserAuthCode>? _codes;
  StreamSubscription<BrowserAuthFailure>? _failures;
  bool _disposed = false;
  int _generation = 0;

  Future<void>? _initialization;
  Future<void> initialize() => _initialization ??= _initialize();

  Future<void> _initialize() async {
    if (_codes != null) return;
    _codes = browser.codes.listen((code) {
      final generation = _generation;
      _enqueue(() async {
        if (code.intent['type'] == 'account') {
          accountOnly = true;
          _target = null;
          _set(HostedFlowStage.working);
          await account.exchange(code: code.code, verifier: code.verifier);
          if (_disposed || generation != _generation) return;
          await _continue(generation);
          return;
        }
        accountOnly = false;
        final target = _decode(code.intent);
        if (target == null) throw BrowserAuthFailure.restartLogin;
        _target = target;
        _set(HostedFlowStage.working);
        await account.exchange(code: code.code, verifier: code.verifier);
        if (generation == _generation) await _continue(generation);
      });
    });
    _failures = browser.failures.listen((failure) {
      if (_disposed) return;
      if (accountOnly) {
        if (stage != HostedFlowStage.signIn) return;
        if (failure == BrowserAuthFailure.cancelled) {
          unawaited(leave());
        } else {
          _set(HostedFlowStage.blocked, HostedFlowProblem.retry);
        }
        return;
      }
      if (_target == null) {
        if (_generation == 0) {
          _set(HostedFlowStage.blocked, HostedFlowProblem.retry);
        }
        return;
      }
      if (stage != HostedFlowStage.signIn) return;
      if (failure == BrowserAuthFailure.cancelled) {
        unawaited(leave());
      } else {
        _set(HostedFlowStage.blocked, HostedFlowProblem.retry);
      }
    });
    await _enqueue(() async {
      final generation = _generation;
      await browser.initialize();
      if (_disposed || generation != _generation) return;
      if (_target == null) {
        final retained = await browser.retainedIntent();
        if (_disposed || generation != _generation) return;
        if (retained != null && _target == null) {
          if (retained['type'] == 'account') {
            accountOnly = true;
            _set(HostedFlowStage.idle);
          } else {
            _target = _decode(retained);
          }
          if (_target != null && stage == HostedFlowStage.idle) {
            _set(HostedFlowStage.signIn);
          }
        }
      }
    });
  }

  /// Explicit Connect completes identity setup and discovers owned communities.
  /// Passive startup restore only checks the saved session.
  Future<void> signInAccount({bool openBrowser = true}) {
    if (_target != null ||
        stage == HostedFlowStage.working ||
        stage == HostedFlowStage.signIn) {
      return Future.value();
    }
    final generation = openBrowser ? ++_generation : _generation;
    if (openBrowser) {
      accountOnly = true;
      _clearCommunityBatch();
      _set(HostedFlowStage.working);
    }
    return _enqueue(() async {
      if (!openBrowser) {
        if (_target != null || stage != HostedFlowStage.idle) return;
      }
      final restored = await account.restore();
      if (_disposed || generation != _generation) return;
      if (!openBrowser) {
        // Mounting the welcome page must not create/bind/back up a key.
        _set(HostedFlowStage.idle);
      } else if (restored != null) {
        await _continue(generation);
      } else {
        _set(HostedFlowStage.signIn);
        await browser.start({'type': 'account'});
      }
    });
  }

  Future<void> start(BuzzDeepLink target) {
    if (target is! NostrBindDeepLink &&
        target is! AddCommunityDeepLink &&
        target is! InviteDeepLink) {
      throw ArgumentError('Unsupported hosted target');
    }
    final generation = ++_generation;
    accountOnly = false;
    _clearCommunityBatch();
    _target = target;
    _set(HostedFlowStage.working);
    return _enqueue(() async {
      if (generation != _generation) return;
      if (!_acceptTarget(target)) return;
      final restored = await account.restore();
      if (generation != _generation) return;
      if (restored == null) {
        _set(HostedFlowStage.signIn);
        await browser.start(_encode(target));
      } else {
        await _continue(generation);
      }
    });
  }

  Future<void> retry() async {
    final target = _target;
    if (target == null && !accountOnly) return;
    final generation = ++_generation;
    try {
      await browser.cancel();
      if (_disposed || generation != _generation) return;
      if (target != null) {
        await start(target);
      } else {
        _set(HostedFlowStage.working);
        await _enqueue(() async {
          final restored = await account.restore();
          if (_disposed || generation != _generation) return;
          if (restored == null) {
            _set(HostedFlowStage.signIn);
            await browser.start({'type': 'account'});
          } else {
            await _continue(generation);
          }
        });
      }
    } catch (_) {
      if (!_disposed && generation == _generation) {
        _set(HostedFlowStage.blocked, HostedFlowProblem.retry);
      }
    }
  }

  /// Resume only an offered public identity; reread local and cloud facts first.
  Future<void> chooseIdentity(String pubkey) {
    if ((_target == null && !accountOnly) ||
        stage != HostedFlowStage.chooseIdentity ||
        !identityChoices.any((item) => item.pubkey == pubkey)) {
      return Future.value();
    }
    final generation = ++_generation;
    _set(HostedFlowStage.working);
    return _enqueue(() => _continue(generation, selectedPubkey: pubkey));
  }

  Future<void> _continue(int generation, {String? selectedPubkey}) async {
    final target = _target;
    final session = account.account;
    if ((target == null && !accountOnly) || session == null) {
      throw AccountFailure.signedOut;
    }
    if (target != null && !_acceptTarget(target)) return;
    if (target == null && _batchSubject != session.subject) {
      _clearCommunityBatch();
      _batchSubject = session.subject;
    }
    var resolved = await identity.resolve(
      isCurrent: () => !_disposed && generation == _generation,
      selectedPubkey: selectedPubkey,
    );
    if (target is! NostrBindDeepLink) {
      for (var step = 0; step < 2; step++) {
        if (_disposed || generation != _generation) return;
        if (resolved.state == HostedIdentityState.needsBinding &&
            resolved.pubkey != null) {
          final response = await account.post(
            '/v1/buzz/nostr-identities/challenge',
            {'origin': HostedEnvironment.origin.toString()},
          );
          if (_disposed || generation != _generation) return;
          final fields = <String, String>{
            'audience': 'buzz:nostr-identity',
            'action': 'bind_nostr_identity',
            'protocol': 'buzz-nostr-identity',
            'version': '1',
            'return': 'browser_fragment_v1',
            'callback_url': HostedEnvironment.origin
                .resolve('/buzz')
                .toString(),
          };
          for (final key in [
            'challenge_id',
            'nonce',
            'verification_code',
            'origin',
            'expires_at',
          ]) {
            final value = response[key];
            if (value is! String) throw AccountFailure.invalidResponse;
            fields[key] = value;
          }
          final request = parseNostrBindDeepLink(
            Uri(scheme: 'buzz', host: 'nostr-bind', queryParameters: fields),
          );
          if (response['error'] != null || request == null) {
            throw AccountFailure.invalidResponse;
          }
          await WebConnectBindingService().complete(
            request: request,
            subject: session.subject,
            expectedPubkey: resolved.pubkey!,
            withKeys: store.withKeys,
            verify: _verifyBinding,
          );
        } else if (resolved.state == HostedIdentityState.needsBackup) {
          await HostedBackupService(account, store, hpke: hpke).backup();
        } else {
          break;
        }
        if (_disposed || generation != _generation) return;
        resolved = await identity.resolve(
          isCurrent: () => !_disposed && generation == _generation,
        );
      }
    }
    if (generation != _generation || _disposed) return;
    if (resolved.state == HostedIdentityState.needsSelection) {
      identityChoices = List.unmodifiable(resolved.choices);
      _set(HostedFlowStage.chooseIdentity);
      return;
    }
    final pubkey = resolved.pubkey;
    if (target is NostrBindDeepLink &&
        pubkey != null &&
        {
          HostedIdentityState.ready,
          HostedIdentityState.needsBinding,
          HostedIdentityState.needsBackup,
        }.contains(resolved.state)) {
      final returnTo = await WebConnectBindingService().complete(
        request: target,
        subject: session.subject,
        expectedPubkey: pubkey,
        withKeys: store.withKeys,
        verify: _verifyBinding,
      );
      if (generation != _generation || _disposed) return;
      if (!await _openWeb(returnTo)) throw BrowserAuthFailure.unavailable;
      await browser.clearIntent();
      _set(HostedFlowStage.browserReturned);
      return;
    }
    if (resolved.state != HostedIdentityState.ready || pubkey == null) {
      final failure = switch (resolved.state) {
        HostedIdentityState.needsBackup => HostedFlowProblem.backupRequired,
        HostedIdentityState.needsBinding => HostedFlowProblem.bindingRequired,
        HostedIdentityState.mismatch => HostedFlowProblem.identityMismatch,
        _ => HostedFlowProblem.identityUnavailable,
      };
      _set(HostedFlowStage.blocked, failure);
      return;
    }
    if (target == null) {
      await _continueCommunities(generation, session.subject, pubkey);
      return;
    }
    await enterCommunity(
      target,
      session.subject,
      pubkey,
      () => !_disposed && generation == _generation,
    );
    if (generation != _generation || _disposed) return;
    await browser.clearIntent();
    _set(HostedFlowStage.complete);
  }

  void selectCommunity(String relay, bool selected) {
    if (stage != HostedFlowStage.chooseCommunities ||
        !communities.any((item) => item.relayUrl == relay)) {
      return;
    }
    selectedRelays = Set.unmodifiable({
      ...selectedRelays.where((item) => item != relay),
      if (selected) relay,
    });
    notifyListeners();
  }

  Future<void> addSelectedCommunities() {
    if (!accountOnly ||
        stage != HostedFlowStage.chooseCommunities ||
        selectedRelays.isEmpty) {
      return Future.value();
    }
    _addingCommunities = true;
    final generation = ++_generation;
    _set(HostedFlowStage.working);
    return _enqueue(() => _continue(generation));
  }

  /// Keep completed additions and let the user omit a failing target.
  void backToCommunityChoices() {
    if (!canChangeCommunities) return;
    ++_generation;
    communities = List.unmodifiable(
      communities.where((item) => !_addedRelays.contains(item.relayUrl)),
    );
    selectedRelays = Set.unmodifiable(selectedRelays.difference(_addedRelays));
    _conflictingRelay = null;
    addingCommunityName = null;
    _set(HostedFlowStage.chooseCommunities);
  }

  Future<void> skipConflictingCommunity() {
    final relay = _conflictingRelay;
    if (stage != HostedFlowStage.communityConflict || relay == null) {
      return Future.value();
    }
    selectedRelays = Set.unmodifiable(
      selectedRelays.where((item) => item != relay),
    );
    _conflictingRelay = null;
    final generation = ++_generation;
    _set(HostedFlowStage.working);
    return _enqueue(() => _continue(generation));
  }

  Future<void> _continueCommunities(
    int generation,
    String subject,
    String pubkey,
  ) async {
    bool current() =>
        !_disposed &&
        generation == _generation &&
        account.account?.subject == subject;
    if (!_addingCommunities) {
      final listed = await loadHostedCommunities(account);
      if (!current()) return;
      communities = listed;
      selectedRelays = Set.unmodifiable(listed.map((item) => item.relayUrl));
      if (listed.isEmpty) {
        await leave(noCommunities: true);
        return;
      }
      if (listed.length != 1) {
        _set(HostedFlowStage.chooseCommunities);
        return;
      }
      _addingCommunities = true;
    }
    for (final community in communities) {
      if (!current()) return;
      if (!selectedRelays.contains(community.relayUrl) ||
          _addedRelays.contains(community.relayUrl)) {
        continue;
      }
      addingCommunityName = community.name;
      notifyListeners();
      try {
        await enterCommunity(community, subject, pubkey, current);
      } on SavedCommunityIdentityConflict {
        if (!current()) return;
        _conflictingRelay = community.relayUrl;
        _set(HostedFlowStage.communityConflict);
        return;
      }
      if (!current()) return;
      _addedRelays.add(community.relayUrl);
    }
    if (!current()) return;
    addingCommunityName = null;
    if (_addedRelays.isEmpty) {
      communities = List.unmodifiable(
        communities.where((item) => selectedRelays.contains(item.relayUrl)),
      );
      await leave(noCommunities: true);
      return;
    }
    await browser.clearIntent();
    if (current()) _set(HostedFlowStage.complete);
  }

  bool _acceptTarget(BuzzDeepLink target) {
    final relay = switch (target) {
      InviteDeepLink(:final relayUrl) => relayUrl,
      AddCommunityDeepLink(:final relayUrl) => relayUrl,
      _ => null,
    };
    if (relay != null && !HostedEnvironment.supportsRelay(Uri.parse(relay))) {
      _set(HostedFlowStage.blocked, HostedFlowProblem.unsupportedEnvironment);
      return false;
    }
    return true;
  }

  Future<String> _verifyBinding({
    required String subject,
    required String challengeId,
    required String nonce,
    required String signedPayload,
  }) async {
    if (account.account?.subject != subject) {
      throw AccountFailure.differentAccount;
    }
    final response = await account.post('/v1/buzz/nostr-identities/verify', {
      'challenge_id': challengeId,
      'nonce': nonce,
      'signed_payload': signedPayload,
    });
    if (account.account?.subject != subject) {
      throw AccountFailure.differentAccount;
    }
    final bound = response['identity'];
    if (response['error'] != null ||
        bound is! Map ||
        bound['pubkey_hex'] is! String) {
      throw AccountFailure.invalidResponse;
    }
    return bound['pubkey_hex'] as String;
  }

  Future<void> _enqueue(Future<void> Function() operation) {
    final generation = _generation;
    final result = _tail.then((_) async {
      if (_disposed || generation != _generation) return;
      try {
        await operation();
      } catch (error) {
        if (!_disposed && generation == _generation) {
          _set(HostedFlowStage.blocked, _problemFor(error));
        }
      }
    });
    _tail = result;
    return result;
  }

  static HostedFlowProblem _problemFor(Object error) => switch (error) {
    AccountFailure.denied => HostedFlowProblem.denied,
    AccountFailure.rejected ||
    HostedBackupFailure.rejected => HostedFlowProblem.requestRejected,
    HostedBackupFailure.unavailable => HostedFlowProblem.backupUnavailable,
    AccountFailure.differentAccount => HostedFlowProblem.differentAccount,
    AccountIdentityFailure.unreadable ||
    AccountIdentityFailure.invalidKey ||
    AccountFailure.storage ||
    HostedBackupFailure.storage => HostedFlowProblem.identityUnreadable,
    SavedCommunityIdentityConflict() ||
    AccountIdentityFailure.mismatch ||
    AccountIdentityFailure.occupied ||
    HostedBackupFailure.identityMismatch ||
    HpkeBackupFailure.identityMismatch => HostedFlowProblem.identityMismatch,
    HpkeBackupFailure.unavailable => HostedFlowProblem.backupUnavailable,
    AccountFailure.invalidResponse ||
    HostedBackupFailure.invalidResponse ||
    HpkeBackupFailure.invalidEnrollment => HostedFlowProblem.invalidResponse,
    _ => HostedFlowProblem.retry,
  };

  void _set(HostedFlowStage next, [HostedFlowProblem? failure]) {
    if (_disposed) return;
    if (next != HostedFlowStage.chooseIdentity) identityChoices = const [];
    stage = next;
    problem = failure;
    notifyListeners();
  }

  bool noCommunitiesToAdd = false;

  void _clearCommunityBatch() {
    noCommunitiesToAdd = false;
    communities = const [];
    selectedRelays = const {};
    _addedRelays.clear();
    _batchSubject = null;
    _conflictingRelay = null;
    _addingCommunities = false;
    addingCommunityName = null;
  }

  Future<void> leave({bool noCommunities = false}) async {
    ++_generation;
    final generation = _generation;
    _target = null;
    accountOnly = false;
    _clearCommunityBatch();
    noCommunitiesToAdd = noCommunities;
    try {
      await browser.cancel();
      if (_disposed || generation != _generation) return;
      await browser.clearIntent();
      if (generation == _generation) _set(HostedFlowStage.idle);
    } catch (_) {
      if (generation == _generation) {
        _set(HostedFlowStage.blocked, HostedFlowProblem.retry);
      }
    }
  }

  static Map<String, String> _encode(BuzzDeepLink target) {
    if (target is NostrBindDeepLink) return {'type': 'bind', ...target.fields};
    if (target is AddCommunityDeepLink) {
      return {'type': 'add', 'relay': target.relayUrl, 'name': target.name};
    }
    final invite = target as InviteDeepLink;
    return {
      'type': 'invite',
      'relay': invite.relayUrl,
      'code': invite.code,
      if (invite.policyReceipt != null) 'policy_receipt': invite.policyReceipt!,
    };
  }

  static BuzzDeepLink? _decode(Map<String, String> intent) {
    final fields = Map<String, String>.of(intent)..remove('type');
    final host = switch (intent['type']) {
      'bind' => 'nostr-bind',
      'add' => 'add-community',
      'invite' => 'join',
      _ => '',
    };
    return parseBuzzDeepLink(
      Uri(scheme: 'buzz', host: host, queryParameters: fields),
    );
  }

  @override
  void dispose() {
    _disposed = true;
    ++_generation;
    unawaited(_codes?.cancel());
    unawaited(_failures?.cancel());
    super.dispose();
  }
}
