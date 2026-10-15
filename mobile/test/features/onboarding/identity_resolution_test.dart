import 'package:buzz/features/onboarding/identity_resolution.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  test('matching recovered backup survives save without another upload', () {
    expect(
      resolveHostedIdentity(
        const IdentityFacts(
          local: IdentityRead.available,
          localKey: 'AB',
          bindingKnown: true,
          boundKey: 'ab',
          recovery: RecoveryRead.available,
          recoveredKey: 'Ab',
        ),
      ),
      IdentityAction.authorize,
    );
  });

  test('ambiguous recovery cannot select an identity or create one', () {
    expect(
      resolveHostedIdentity(
        const IdentityFacts(
          local: IdentityRead.absent,
          bindingKnown: true,
          recovery: RecoveryRead.ambiguous,
        ),
      ),
      IdentityAction.blockRecovery,
    );
  });

  test('recovered and local conflicts stop; unbound unique recovery saves', () {
    expect(
      resolveHostedIdentity(
        const IdentityFacts(
          local: IdentityRead.absent,
          bindingKnown: true,
          boundKey: 'K',
          recovery: RecoveryRead.available,
          recoveredKey: 'X',
        ),
      ),
      IdentityAction.conflict,
    );
    expect(
      resolveHostedIdentity(
        const IdentityFacts(
          local: IdentityRead.available,
          localKey: 'K',
          bindingKnown: true,
          boundKey: 'X',
        ),
      ),
      IdentityAction.conflict,
    );
    expect(
      resolveHostedIdentity(
        const IdentityFacts(
          local: IdentityRead.absent,
          bindingKnown: true,
          recovery: RecoveryRead.available,
          recoveredKey: 'K',
        ),
      ),
      IdentityAction.save,
    );
  });

  test(
    'paired keys never cause generation and cannot resolve local ambiguity',
    () {
      for (final recovery in RecoveryRead.values) {
        expect(
          resolveHostedIdentity(
            IdentityFacts(
              local: IdentityRead.absent,
              pairedKey: 'K',
              bindingKnown: true,
              recovery: recovery,
            ),
          ),
          IdentityAction.save,
        );
      }
      expect(
        resolveHostedIdentity(
          const IdentityFacts(
            local: IdentityRead.absent,
            pairedKey: 'K',
            ambiguousLocalKeys: true,
            bindingKnown: true,
          ),
        ),
        IdentityAction.resolveAmbiguity,
      );
    },
  );

  test('generation requires confirmed local absence and empty recovery', () {
    for (final local in IdentityRead.values) {
      for (final recovery in RecoveryRead.values) {
        for (final bindingKnown in [false, true]) {
          for (final binding in [null, 'K', 'X']) {
            final result = resolveHostedIdentity(
              IdentityFacts(
                local: local,
                localKey: local == IdentityRead.available ? 'K' : null,
                bindingKnown: bindingKnown,
                boundKey: binding,
                recovery: recovery,
                recoveredKey: recovery == RecoveryRead.available ? 'K' : null,
              ),
            );
            expect(
              result == IdentityAction.generate,
              local == IdentityRead.absent &&
                  recovery == RecoveryRead.empty &&
                  bindingKnown &&
                  binding == null,
              reason: '$local / $recovery / $bindingKnown / $binding',
            );
          }
        }
      }
    }
  });

  test(
    'local key is reused without calling recovery, even when unavailable',
    () {
      for (final recovery in RecoveryRead.values) {
        expect(
          resolveHostedIdentity(
            IdentityFacts(
              local: IdentityRead.available,
              localKey: 'K',
              bindingKnown: true,
              recovery: recovery,
            ),
          ),
          IdentityAction.bind,
        );
      }
    },
  );

  test('paired key must match local and account before save or backup', () {
    expect(
      resolveHostedIdentity(
        const IdentityFacts(
          local: IdentityRead.available,
          localKey: 'K',
          pairedKey: 'X',
        ),
      ),
      IdentityAction.conflict,
    );
    expect(
      resolveHostedIdentity(
        const IdentityFacts(
          local: IdentityRead.absent,
          pairedKey: 'K',
          bindingKnown: true,
          boundKey: 'X',
        ),
      ),
      IdentityAction.conflict,
    );
    expect(
      resolveHostedIdentity(
        const IdentityFacts(
          local: IdentityRead.absent,
          pairedKey: 'K',
          bindingKnown: true,
          boundKey: 'K',
        ),
      ),
      IdentityAction.save,
    );
  });

  test(
    'backup confirmation only permits authorization for the matching key',
    () {
      for (final receipt in [null, 'X', 'K']) {
        expect(
          resolveHostedIdentity(
            IdentityFacts(
              local: IdentityRead.available,
              localKey: 'K',
              bindingKnown: true,
              boundKey: 'K',
              backupConfirmedForKey: receipt,
            ),
          ),
          receipt == 'K' ? IdentityAction.authorize : IdentityAction.backup,
        );
      }
    },
  );

  test('partial recovery and missing bound key cannot lead to generation', () {
    for (final recovery in [
      RecoveryRead.partial,
      RecoveryRead.unavailable,
      RecoveryRead.empty,
    ]) {
      expect(
        resolveHostedIdentity(
          IdentityFacts(
            local: IdentityRead.absent,
            bindingKnown: true,
            boundKey: 'K',
            recovery: recovery,
          ),
        ),
        IdentityAction.blockRecovery,
      );
    }
  });

  test('ambiguous existing identities require input', () {
    expect(
      resolveHostedIdentity(
        const IdentityFacts(
          local: IdentityRead.absent,
          ambiguousLocalKeys: true,
          bindingKnown: true,
          recovery: RecoveryRead.empty,
        ),
      ),
      IdentityAction.resolveAmbiguity,
    );
  });

  test('inconsistent storage facts cannot create or use a key', () {
    expect(
      resolveHostedIdentity(
        const IdentityFacts(
          local: IdentityRead.available,
          bindingKnown: true,
          recovery: RecoveryRead.empty,
        ),
      ),
      IdentityAction.retryStorage,
    );
    expect(
      resolveHostedIdentity(
        const IdentityFacts(
          local: IdentityRead.absent,
          localKey: 'K',
          bindingKnown: true,
        ),
      ),
      IdentityAction.retryStorage,
    );
  });
}
