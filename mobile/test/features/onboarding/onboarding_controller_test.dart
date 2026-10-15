import 'dart:async';
import 'package:buzz/features/onboarding/onboarding_controller.dart';
import 'package:buzz/features/onboarding/onboarding_entry.dart';
import 'package:buzz/features/onboarding/identity_resolution.dart';
import 'package:flutter_test/flutter_test.dart';
import '../../../tool/onboarding/preview_services.dart';

class FastServices extends PreviewServices {
  FastServices(super.scenario);
  @override
  Future<void> pause(String operation) async {
    operations.add(operation);
  }
}

class DelayedLogin extends FastServices {
  DelayedLogin() : super(PreviewScenario.newUser);
  final pending = Completer<void>();
  @override
  Future<void> signIn(OnboardingTarget target) => pending.future;
}

class ExpiredServices extends FastServices {
  ExpiredServices() : super(PreviewScenario.existingKey);
  bool expired = true;
  @override
  Future<void> enterCommunity(OnboardingTarget target) async {
    if (expired) {
      expired = false;
      throw SetupFailure.expired;
    }
    await super.enterCommunity(target);
  }
}

class DelayedWrite extends FastServices {
  DelayedWrite() : super(PreviewScenario.newUser);
  final started = Completer<void>();
  final finish = Completer<void>();
  @override
  Future<void> applyIdentity(
    OnboardingTarget target,
    IdentityAction action,
  ) async {
    if (action == IdentityAction.generate) {
      started.complete();
      await finish.future;
    }
    await super.applyIdentity(target, action);
  }
}

void main() {
  test(
    'cancelled durable write settles before next attempt reads identity',
    () async {
      final service = DelayedWrite();
      final controller = OnboardingController(service)
        ..setTarget('example.communities.buzz.xyz');
      final first = controller.login();
      await service.started.future;
      controller.leave();
      controller.choose(OnboardingIntent.join);
      controller.setTarget('example.communities.buzz.xyz');
      final second = controller.login();
      await Future<void>.delayed(Duration.zero);
      expect(service.operations.where((op) => op == 'login').length, 1);
      service.finish.complete();
      await first;
      await second;
      expect(service.operations.where((op) => op == 'generate').length, 1);
      expect(controller.stage, OnboardingStage.complete);
    },
  );
  test('expired session returns to login and preserves identity', () async {
    final service = ExpiredServices();
    final controller = OnboardingController(service)
      ..setTarget('example.communities.buzz.xyz');
    await controller.login();
    expect(controller.stage, OnboardingStage.login);
    expect(service.local, 'key-k');
    await controller.login();
    expect(controller.stage, OnboardingStage.complete);
    expect(service.operations.where((op) => op == 'login').length, 2);
    expect(service.operations, isNot(contains('generate')));
  });
  test('paired mismatch never saves or admits', () async {
    final service = FastServices(PreviewScenario.pairedMismatch);
    final controller = OnboardingController(service);
    await controller.pair('demo-hosted');
    await controller.login();
    expect(controller.stage, OnboardingStage.problem);
    expect(service.local, 'key-k');
    expect(service.operations, isNot(contains('admission')));
    expect(service.operations, isNot(contains('save')));
  });
  test('denial cannot be retried', () async {
    final service = FastServices(PreviewScenario.denied);
    final controller = OnboardingController(service)
      ..setTarget('example.communities.buzz.xyz');
    await controller.login();
    expect(controller.canRetry, isFalse);
    final count = service.operations.length;
    await controller.retry();
    expect(service.operations.length, count);
  });
  test('new hosted join saves, binds and backs up before admission', () async {
    final service = FastServices(PreviewScenario.newUser);
    final controller = OnboardingController(service);
    controller.choose(OnboardingIntent.join);
    controller.setTarget('example.communities.buzz.xyz');
    expect(controller.stage, OnboardingStage.login);
    expect(service.operations, isEmpty);
    await controller.login();
    expect(service.operations, [
      'login',
      'readBinding',
      'recover',
      'generate',
      'bind',
      'backup',
      'admission',
    ]);
    expect(controller.stage, OnboardingStage.complete);
  });
  test(
    'existing and recovered identities never generate a replacement',
    () async {
      for (final scenario in [
        PreviewScenario.existingKey,
        PreviewScenario.recovered,
      ]) {
        final service = FastServices(scenario);
        final controller = OnboardingController(service)
          ..setTarget('example.communities.buzz.xyz');
        await controller.login();
        expect(controller.stage, OnboardingStage.complete);
        expect(service.operations, isNot(contains('generate')));
        if (scenario == PreviewScenario.recovered) {
          expect(service.operations, isNot(contains('backup')));
        }
      }
    },
  );
  test('self hosted pairing never calls hosted account operations', () async {
    final service = FastServices(PreviewScenario.newUser);
    final controller = OnboardingController(service);
    await controller.pair('demo-self-hosted');
    expect(service.operations, ['pair', 'selfHosted', 'admission']);
    expect(controller.stage, OnboardingStage.complete);
  });
  test('hosted pairing waits for login after transfer', () async {
    final service = FastServices(PreviewScenario.newUser);
    final controller = OnboardingController(service);
    await controller.pair('demo-hosted');
    expect(controller.stage, OnboardingStage.login);
    expect(service.operations, ['pair']);
    await controller.login();
    expect(service.operations, isNot(contains('generate')));
    expect(controller.stage, OnboardingStage.complete);
  });
  test('backup retry preserves key and cannot admit early', () async {
    final service = FastServices(PreviewScenario.backupFailure);
    final controller = OnboardingController(service)
      ..setTarget('example.communities.buzz.xyz');
    await controller.login();
    expect(controller.stage, OnboardingStage.problem);
    expect(service.operations, isNot(contains('admission')));
    final key = service.local;
    await controller.retry();
    expect(service.local, key);
    expect(service.operations.where((value) => value == 'generate').length, 1);
    expect(controller.stage, OnboardingStage.complete);
  });
  test('mismatch and unavailable recovery block admission', () async {
    for (final scenario in [
      PreviewScenario.mismatch,
      PreviewScenario.recoveryUnavailable,
    ]) {
      final service = FastServices(scenario);
      final controller = OnboardingController(service)
        ..setTarget('example.communities.buzz.xyz');
      await controller.login();
      expect(controller.stage, OnboardingStage.problem);
      expect(service.operations, isNot(contains('admission')));
      expect(service.operations, isNot(contains('generate')));
    }
  });
  test('cancellation fences a late login and serializes attempts', () async {
    final service = DelayedLogin();
    final controller = OnboardingController(service)
      ..setTarget('example.communities.buzz.xyz');
    final login = controller.login();
    controller.leave();
    controller.choose(OnboardingIntent.create);
    expect(controller.target, isNull);
    service.pending.complete();
    await login;
    expect(controller.stage, OnboardingStage.target);
    expect(service.operations, isEmpty);
    controller.choose(OnboardingIntent.create);
    controller.setTarget('new.communities.buzz.xyz');
    expect(controller.target!.create, isTrue);
  });
  test(
    'URL policy matches host boundaries, rejects credential-bearing URLs',
    () {
      final controller = OnboardingController(
        FastServices(PreviewScenario.newUser),
      );
      controller.setTarget('https://user:pass@example.communities.buzz.xyz');
      expect(controller.stage, OnboardingStage.target);
      expect(controller.target, isNull);
      controller.setTarget('buzz.xyz.attacker.example');
      expect(controller.target!.hosted, isFalse);
    },
  );
}
