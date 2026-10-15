import 'package:buzz/features/onboarding/identity_resolution.dart';
import 'package:buzz/features/onboarding/onboarding_controller.dart';

enum PreviewScenario {
  newUser,
  recovered,
  existingKey,
  mismatch,
  recoveryUnavailable,
  backupFailure,
  denied,
  pairedMismatch,
}

/// Memory-only fixture. Never accepts real private keys or calls a service.
class PreviewServices implements OnboardingServices {
  PreviewServices(this.scenario) {
    if (scenario == PreviewScenario.existingKey ||
        scenario == PreviewScenario.mismatch ||
        scenario == PreviewScenario.pairedMismatch) {
      local = 'key-k';
    }
  }
  final PreviewScenario scenario;
  int _epoch = 0;
  @override
  void cancel() {
    _epoch++;
  }

  String? paired;
  String? selfHostedKey;
  String? local;
  String? bound;
  String? receipt;
  bool bindingKnown = false;
  RecoveryRead recovery = RecoveryRead.unknown;
  String? recovered;
  bool failedBackup = false;
  final operations = <String>[];
  Future<void> pause(String operation) async {
    final epoch = _epoch;
    operations.add(operation);
    await Future<void>.delayed(const Duration(milliseconds: 250));
    if (epoch != _epoch) throw StateError('Cancelled');
  }

  @override
  Future<void> signIn(OnboardingTarget target) => pause('login');
  @override
  Future<IdentityFacts> readIdentity(OnboardingTarget target) async =>
      IdentityFacts(
        local: local == null ? IdentityRead.absent : IdentityRead.available,
        localKey: local,
        pairedKey: paired,
        bindingKnown: bindingKnown,
        boundKey: bound,
        recovery: recovery,
        recoveredKey: recovered,
        backupConfirmedForKey: receipt,
      );
  @override
  Future<void> applyIdentity(
    OnboardingTarget target,
    IdentityAction action,
  ) async {
    await pause(action.name);
    switch (action) {
      case IdentityAction.readBinding:
        bindingKnown = true;
        bound = scenario == PreviewScenario.mismatch
            ? 'key-x'
            : scenario == PreviewScenario.recovered
            ? 'key-k'
            : null;
      case IdentityAction.recover:
        recovery = scenario == PreviewScenario.recoveryUnavailable
            ? RecoveryRead.unavailable
            : scenario == PreviewScenario.recovered
            ? RecoveryRead.available
            : RecoveryRead.empty;
        recovered = recovery == RecoveryRead.available ? 'key-k' : null;
      case IdentityAction.generate:
        local = 'key-k';
      case IdentityAction.save:
        local = paired ?? recovered;
        if (recovered == local) receipt = recovered;
      case IdentityAction.bind:
        bound = local;
      case IdentityAction.backup:
        if (scenario == PreviewScenario.backupFailure && !failedBackup) {
          failedBackup = true;
          throw StateError('Simulated backup failure');
        }
        receipt = local;
      default:
        throw StateError('Unexpected operation');
    }
  }

  @override
  Future<void> prepareSelfHosted(OnboardingTarget target) async {
    await pause('selfHosted');
    selfHostedKey ??= paired ?? 'self-key';
  }

  @override
  Future<void> enterCommunity(OnboardingTarget target) async {
    await pause('admission');
    if (scenario == PreviewScenario.denied) {
      throw SetupFailure.denied;
    }
  }

  @override
  Future<void> importKey(String input) async {
    // Do not encourage entering secrets into a demo.
    if (input.trim() != 'demo-key') throw FormatException('Use demo-key');
    await pause('import');
    if (local != null && local != 'key-k') throw StateError('Mismatch');
    paired = 'key-k';
  }

  @override
  Future<OnboardingTarget> pair(String code) async {
    if (!['demo-hosted', 'demo-self-hosted'].contains(code.trim())) {
      throw FormatException('Use a demo code');
    }
    await pause('pair');
    paired = scenario == PreviewScenario.pairedMismatch ? 'key-x' : 'key-k';
    return OnboardingTarget(
      Uri.parse(
        code.trim() == 'demo-hosted'
            ? 'https://example.communities.buzz.xyz'
            : 'https://community.example.org',
      ),
      hosted: code.trim() == 'demo-hosted',
    );
  }
}
