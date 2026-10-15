/// Verified facts supplied by the storage/recovery adapter. Unknown must never
/// be represented by an empty successful result.
enum IdentityRead { unknown, absent, available, unreadable }

enum RecoveryRead { unknown, empty, available, unavailable, partial, ambiguous }

enum IdentityAction {
  retryStorage,
  resolveAmbiguity,
  conflict,
  recover,
  blockRecovery,
  generate,
  save,
  readBinding,
  bind,
  backup,
  authorize,
}

/// Public-key-only inputs for one account and selected identity. Adapters must
/// scope these facts to the same attempt/account before invoking the resolver.
/// Keys must be validated hex. Local/paired/recovered keys must be derived from
/// the actual secret, never trusted from a payload's claimed public-key field.
/// Successful recovery is backup evidence only for this same account/backend.
class IdentityFacts {
  const IdentityFacts({
    this.local = IdentityRead.unknown,
    this.localKey,
    this.pairedKey,
    this.ambiguousLocalKeys = false,
    this.bindingKnown = false,
    this.boundKey,
    this.recovery = RecoveryRead.unknown,
    this.recoveredKey,
    this.backupConfirmedForKey,
  });
  final IdentityRead local;
  final String? localKey;
  final String? pairedKey;
  final bool ambiguousLocalKeys;
  final bool bindingKnown;
  final String? boundKey;
  final RecoveryRead recovery;
  final String? recoveredKey;
  final String? backupConfirmedForKey;
}

/// Decides the next identity operation without generating, replacing, or
/// exporting a secret. Authorization is only a next step, never admission.
IdentityAction resolveHostedIdentity(IdentityFacts facts) {
  if (facts.local == IdentityRead.unknown ||
      facts.local == IdentityRead.unreadable ||
      (facts.local == IdentityRead.available && facts.localKey == null) ||
      (facts.local == IdentityRead.absent && facts.localKey != null)) {
    return IdentityAction.retryStorage;
  }
  if (facts.ambiguousLocalKeys) return IdentityAction.resolveAmbiguity;
  final local = facts.localKey?.toLowerCase();
  final paired = facts.pairedKey?.toLowerCase();
  final bound = facts.boundKey?.toLowerCase();
  final recovered = facts.recoveredKey?.toLowerCase();
  if (local != null && paired != null && local != paired) {
    return IdentityAction.conflict;
  }
  if (!facts.bindingKnown) return IdentityAction.readBinding;
  final candidate = local ?? paired;
  if (candidate != null) {
    if (bound != null && bound != candidate) {
      return IdentityAction.conflict;
    }
    if (local == null) return IdentityAction.save;
    if (bound == null) return IdentityAction.bind;
    final recoveredBackup =
        facts.recovery == RecoveryRead.available && recovered == candidate;
    if (facts.backupConfirmedForKey?.toLowerCase() != candidate &&
        !recoveredBackup) {
      return IdentityAction.backup;
    }
    return IdentityAction.authorize;
  }
  switch (facts.recovery) {
    case RecoveryRead.unknown:
      return IdentityAction.recover;
    case RecoveryRead.unavailable:
    case RecoveryRead.partial:
    case RecoveryRead.ambiguous:
      return IdentityAction.blockRecovery;
    case RecoveryRead.empty:
      return bound == null
          ? IdentityAction.generate
          : IdentityAction.blockRecovery;
    case RecoveryRead.available:
      if (recovered == null) return IdentityAction.blockRecovery;
      if (bound != null && bound != recovered) {
        return IdentityAction.conflict;
      }
      return IdentityAction.save;
  }
}
