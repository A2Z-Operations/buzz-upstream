import 'dart:async';
import 'package:flutter/foundation.dart';
import 'identity_resolution.dart';
import 'community_policy.dart';
import 'onboarding_entry.dart';

enum OnboardingStage {
  welcome,
  target,
  login,
  importKey,
  pairing,
  busy,
  problem,
  complete,
}

/// Immutable attempt context; no secrets belong in widget/controller state.
class OnboardingTarget {
  const OnboardingTarget(
    this.origin, {
    required this.hosted,
    this.create = false,
  });
  final Uri origin;
  final bool hosted;
  final bool create;
}

/// All effectful operations belong to an adapter. A production adapter must
/// persist intent before writes, scope sessions/receipts by account and backend,
/// and reconcile uncertain outcomes. Cancellation must not erase saved keys.
enum SetupFailure { denied, expired }

abstract interface class OnboardingServices {
  /// Fence pending effects synchronously and abort interactive/network work.
  /// Already committed writes remain durable and must be reconciled on retry.
  void cancel();
  Future<IdentityFacts> readIdentity(OnboardingTarget target);
  Future<void> signIn(OnboardingTarget target);
  Future<void> applyIdentity(OnboardingTarget target, IdentityAction action);
  Future<void> prepareSelfHosted(OnboardingTarget target);
  Future<void> enterCommunity(OnboardingTarget target);
  Future<void> importKey(String input);
  Future<OnboardingTarget> pair(String code);
}

/// Single owner of navigation and effect sequencing. It never equates
/// identity readiness with community admission or starts concurrent writes.
class OnboardingController extends ChangeNotifier {
  OnboardingController(this.services);
  final OnboardingServices services;
  OnboardingStage stage = OnboardingStage.welcome;
  OnboardingTarget? target;
  String? message;
  bool canRetry = true;
  String progress = '';
  bool creating = false;
  bool _signedIn = false;
  bool _running = false;
  Future<void> _settled = Future<void>.value();
  bool _disposed = false;
  int _generation = 0;
  Future<void> Function()? _retry;

  void _show(OnboardingStage next, [String? text]) {
    if (_disposed) return;
    stage = next;
    message = text;
    notifyListeners();
  }

  void choose(OnboardingIntent intent) {
    if (_running) return;
    creating = intent == OnboardingIntent.create;
    _signedIn = false;
    target = null;
    _show(switch (intent) {
      OnboardingIntent.pair => OnboardingStage.pairing,
      OnboardingIntent.importKey => OnboardingStage.importKey,
      _ => OnboardingStage.target,
    });
  }

  void setTarget(String input) {
    if (_running) return;
    final address = CommunityPolicy.parse(input);
    if (address == null) {
      _show(
        OnboardingStage.target,
        'Enter a valid community address using its standard secure port.',
      );
      return;
    }
    final hosted = address.hosted;
    if (creating && !hosted) {
      _show(
        OnboardingStage.target,
        'Hosted creation needs a Buzz community address.',
      );
      return;
    }
    target = OnboardingTarget(address.origin, hosted: hosted, create: creating);
    _signedIn = false;
    if (hosted) {
      _show(OnboardingStage.login);
    } else {
      reconcile();
    }
  }

  Future<void> _run(
    Future<void> Function(int) task,
    Future<void> Function() retry,
  ) async {
    if (_running || _disposed) return;
    _running = true;
    final previous = _settled;
    final completion = Completer<void>();
    _settled = completion.future;
    final generation = _generation;
    _retry = retry;
    canRetry = true;
    _show(OnboardingStage.busy);
    try {
      // Navigation may move on, but an in-flight durable write must settle
      // before the next attempt reads facts or performs any effects.
      await previous;
      if (!_current(generation)) return;
      await task(generation);
    } on SetupFailure catch (failure) {
      if (_current(generation)) {
        if (failure == SetupFailure.expired) {
          _signedIn = false;
          _show(OnboardingStage.login);
        } else {
          canRetry = false;
          _show(
            OnboardingStage.problem,
            'This account cannot access this community. Contact the community owner or use another account.',
          );
        }
      }
    } catch (_) {
      // Adapter exceptions can contain tokens or secrets; never render them.
      if (_current(generation)) {
        _show(OnboardingStage.problem, 'We couldn’t finish setup. Try again.');
      }
    } finally {
      completion.complete();
      if (_current(generation)) _running = false;
      if (!_disposed) notifyListeners();
    }
  }

  bool _current(int generation) => !_disposed && generation == _generation;
  bool get operationPending => _running;

  Future<void> login() => _run((generation) async {
    progress = 'Signing in';
    await services.signIn(target!);
    if (!_current(generation)) return;
    _signedIn = true;
    _retry = reconcile;
    await _reconcile(generation);
  }, login);

  Future<void> importIdentity(String input) => _run(
    (generation) async {
      progress = 'Checking your identity';
      await services.importKey(input);
      if (_current(generation)) _show(OnboardingStage.target);
    },
    () async {
      _show(OnboardingStage.importKey);
    },
  );

  Future<void> pair(String code) => _run(
    (generation) async {
      progress = 'Pairing with desktop';
      final destination = await services.pair(code);
      if (!_current(generation)) return;
      target = destination;
      _signedIn = false;
      if (destination.hosted) {
        _show(OnboardingStage.login);
      } else {
        await _reconcile(generation);
      }
    },
    () async {
      _show(OnboardingStage.pairing);
    },
  );

  Future<void> reconcile() => _run(_reconcile, reconcile);

  Future<void> _reconcile(int generation) async {
    final destination = target;
    if (destination == null) {
      _show(OnboardingStage.target);
      return;
    }
    if (destination.hosted && !_signedIn) {
      _show(OnboardingStage.login);
      return;
    }
    if (!destination.hosted) {
      progress = 'Preparing your identity';
      notifyListeners();
      await services.prepareSelfHosted(destination);
      if (!_current(generation)) return;
    } else {
      // Bound reconciliation; a non-progressing adapter cannot spin forever.
      var ready = false;
      for (var step = 0; step < 12; step++) {
        if (!_current(generation)) return;
        final facts = await services.readIdentity(destination);
        if (!_current(generation)) return;
        final action = resolveHostedIdentity(facts);
        final error = switch (action) {
          IdentityAction.retryStorage =>
            'Your saved identity could not be read. Retry when secure storage is available.',
          IdentityAction.resolveAmbiguity =>
            'Several identities are available. Choose the intended identity before continuing.',
          IdentityAction.conflict =>
            'These identities don’t match. Use the matching account or desktop identity. Nothing has been replaced.',
          IdentityAction.blockRecovery =>
            'Your saved identity could not be recovered safely. Retry or return to pairing or key import.',
          _ => null,
        };
        if (error != null) {
          _show(OnboardingStage.problem, error);
          return;
        }
        if (action == IdentityAction.authorize) {
          ready = true;
          break;
        }
        progress = switch (action) {
          IdentityAction.recover => 'Recovering your identity',
          IdentityAction.generate ||
          IdentityAction.save => 'Saving your identity',
          IdentityAction.backup => 'Backing up your identity',
          _ => 'Connecting your identity',
        };
        notifyListeners();
        await services.applyIdentity(destination, action);
      }
      if (!ready) throw StateError('Adapter made no progress');
    }
    if (!_current(generation)) return;
    progress = destination.create
        ? 'Creating your community'
        : 'Joining your community';
    notifyListeners();
    // Adapter includes scoped assertion, admission/profile and durable local
    // completion. It must return only after all are confirmed.
    await services.enterCommunity(destination);
    if (_current(generation)) _show(OnboardingStage.complete);
  }

  Future<void> retry() async {
    if (!_running && canRetry) await _retry?.call();
  }

  void leave() {
    _generation++;
    services.cancel();
    _running = false;
    _signedIn = false;
    _retry = null;
    _show(OnboardingStage.welcome);
    // Adapter cancellation fences later effects; committed work is retained.
  }

  @override
  void dispose() {
    services.cancel();
    _disposed = true;
    _generation++;
    super.dispose();
  }
}
