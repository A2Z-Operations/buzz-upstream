import 'dart:async';

import 'package:buzz/shared/hosted/builderlab_account.dart';
import 'package:buzz/shared/hosted/browser_auth.dart';
import 'package:buzz/shared/hosted/account_identity_store.dart';
import 'package:buzz/shared/hosted/hosted_community_connection.dart';
import 'package:buzz/shared/hosted/hosted_flow.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';

import '../../helpers/hosted_communities_fixture.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  setUp(() => FlutterSecureStorage.setMockInitialValues({}));

  Future<HostedCommunitiesFixture> fixture({bool backup = true}) async {
    final result = HostedCommunitiesFixture();
    await result.initialize(backup: backup);
    addTearDown(result.dispose);
    return result;
  }

  test(
    'account browser callback continues into discovery after exchange',
    () async {
      final f = await fixture();
      f.rows = [f.row('one'), f.row('two')];
      await f.flow.initialize();
      final selected = Completer<void>();
      f.flow.addListener(() {
        if (f.flow.stage == HostedFlowStage.chooseCommunities &&
            !selected.isCompleted) {
          selected.complete();
        }
      });
      f.browser.callbacks.add(
        BrowserAuthCode('returned-code', 'a' * 43, {'type': 'account'}),
      );
      await selected.future.timeout(const Duration(seconds: 2));
      expect(f.flow.selectedRelays.length, 2);
      expect(f.admitted, isEmpty);
    },
  );

  test(
    'passive session restore cannot discover or enter communities',
    () async {
      final f = await fixture();
      f.rows = [f.row('one')];
      await f.flow.signInAccount(openBrowser: false);
      expect(f.flow.stage, HostedFlowStage.idle);
      expect(f.requests.any((path) => path.endsWith('/current')), isFalse);
      expect(f.admitted, isEmpty);
      await f.flow.signInAccount();
      expect(f.admitted, ['one']);
      expect(f.flow.stage, HostedFlowStage.complete);
    },
  );

  test(
    'filters archived and duplicate hosts before direct single entry',
    () async {
      final f = await fixture();
      f.rows = [f.row('archived', archived: true), f.row('one'), f.row('one')];
      await f.flow.signInAccount();
      expect(f.admitted, ['one']);
    },
  );

  test(
    'multiple communities default selected; only selected ones enter',
    () async {
      final f = await fixture();
      f.rows = [f.row('one'), f.row('two')];
      await f.flow.signInAccount();
      expect(f.flow.stage, HostedFlowStage.chooseCommunities);
      expect(f.flow.selectedRelays.length, 2);
      expect(f.admitted, isEmpty);
      f.flow.selectCommunity('wss://one.communities.buzz.xyz', false);
      await f.flow.addSelectedCommunities();
      expect(f.admitted, ['two']);
    },
  );

  test('empty selection does not complete or admit', () async {
    final f = await fixture();
    f.rows = [f.row('one'), f.row('two')];
    await f.flow.signInAccount();
    for (final relay in f.flow.selectedRelays.toList()) {
      f.flow.selectCommunity(relay, false);
    }
    await f.flow.addSelectedCommunities();
    expect(f.flow.stage, HostedFlowStage.chooseCommunities);
    expect(f.admitted, isEmpty);
  });

  test(
    'partial failure preserves successes and retry only adds remaining',
    () async {
      final f = await fixture();
      f.rows = [f.row('one'), f.row('two'), f.row('three')];
      f.beforeAdmission = (name) async {
        if (name == 'two') throw AccountFailure.unavailable;
      };
      await f.flow.signInAccount();
      await f.flow.addSelectedCommunities();
      expect(f.flow.stage, HostedFlowStage.blocked);
      expect(f.admitted, ['one']);
      expect(f.flow.addedCommunityCount, 1);
      f.beforeAdmission = null;
      await f.flow.retry();
      expect(f.admitted, ['one', 'two', 'three']);
      expect(f.flow.stage, HostedFlowStage.complete);
    },
  );

  test('cancellation fences admission and stale selection', () async {
    final f = await fixture();
    f.rows = [f.row('one')];
    final started = Completer<void>();
    final release = Completer<void>();
    f.beforeAdmission = (_) async {
      started.complete();
      await release.future;
    };
    final pending = f.flow.signInAccount();
    await started.future;
    await f.flow.leave();
    release.complete();
    await pending;
    await f.flow.addSelectedCommunities();
    expect(f.admitted, isEmpty);
    expect(f.flow.stage, HostedFlowStage.idle);
  });

  test('cancelled list cannot display or auto-add its late response', () async {
    final f = await fixture();
    f.rows = [f.row('one'), f.row('two')];
    final started = Completer<void>();
    final release = Completer<void>();
    f.beforeList = () async {
      started.complete();
      await release.future;
    };
    final pending = f.flow.signInAccount();
    await started.future;
    await f.flow.leave();
    release.complete();
    await pending;
    expect(f.admitted, isEmpty);
    expect(f.flow.stage, HostedFlowStage.idle);
    expect(f.flow.communities, isEmpty);
    expect(f.flow.selectedRelays, isEmpty);
  });

  test(
    'zero communities can open creation and refresh into single entry',
    () async {
      final f = await fixture();
      await f.flow.signInAccount();
      expect(f.flow.stage, HostedFlowStage.chooseCommunities);
      await f.flow.openCommunityCreation();
      expect(f.opened, Uri.parse('https://app.builderlab.xyz/buzz'));
      f.rows = [f.row('created')];
      await f.flow.refreshCommunities();
      expect(f.admitted, ['created']);
    },
  );

  test('identity mismatch blocks list and preserves saved key', () async {
    final f = await fixture();
    f.bound = '2' * 64;
    await f.flow.signInAccount();
    expect(f.flow.problem, HostedFlowProblem.identityMismatch);
    expect(
      f.requests.any((path) => path.endsWith('/communities/list')),
      isFalse,
    );
    expect((await f.store.read())?.pubkey, f.keys.public);
    expect(f.admitted, isEmpty);
  });

  test('missing backup cannot unlock discovery or admission', () async {
    final f = await fixture(backup: false);
    await f.flow.signInAccount();
    expect(f.flow.stage, HostedFlowStage.blocked);
    expect(
      f.requests.any((path) => path.endsWith('/communities/list')),
      isFalse,
    );
    expect(f.admitted, isEmpty);
  });

  test(
    'malformed list targets and failed discovery never look like empty lists',
    () async {
      final f = await fixture();
      f.rows = [
        {...f.row('one'), 'normalized_host': 'wss://user@evil.example/path'},
      ];
      await f.flow.signInAccount();
      expect(f.flow.problem, HostedFlowProblem.invalidResponse);
      expect(f.admitted, isEmpty);
      f.rejectList = true;
      await f.flow.signInAccount();
      expect(f.flow.stage, HostedFlowStage.blocked);
      expect(f.flow.problem, HostedFlowProblem.retry);
    },
  );
  test('expired A batch is discarded after browser login as B', () async {
    final f = await fixture();
    await f.flow.initialize();
    f.rows = [f.row('a-one'), f.row('a-two')];
    f.beforeAdmission = (name) async {
      if (name == 'a-two') throw AccountFailure.unavailable;
    };
    await f.flow.signInAccount();
    await f.flow.addSelectedCommunities();
    expect(f.admitted, ['a-one']);
    f.expired = true;
    await f.flow.retry();
    expect(f.flow.stage, HostedFlowStage.signIn);
    expect(f.browser.startedIntent, {'type': 'account'});
    f.expired = false;
    f.subject = 'other-owner';
    f.rows = [f.row('b-one')];
    await f.store.confirmBackup(
      BackupReceipt(
        backend: BuilderlabAccountClient.origin.toString(),
        subject: f.subject,
        pubkey: f.keys.public,
        backupId: 'other-backup',
      ),
    );
    final completed = Completer<void>();
    f.flow.addListener(() {
      if (f.flow.stage == HostedFlowStage.complete && !completed.isCompleted) {
        completed.complete();
      }
    });
    f.browser.callbacks.add(
      BrowserAuthCode('other-code', 'a' * 43, {'type': 'account'}),
    );
    await completed.future.timeout(const Duration(seconds: 2));
    expect(f.admitted, ['a-one', 'b-one']);
  });

  test(
    'conflict asks and explicit skip preserves other selected additions',
    () async {
      final f = await fixture();
      f.rows = [f.row('one'), f.row('two'), f.row('three')];
      f.beforeAdmission = (name) async {
        if (name == 'two') throw const SavedCommunityIdentityConflict();
      };
      await f.flow.signInAccount();
      await f.flow.addSelectedCommunities();
      expect(f.flow.stage, HostedFlowStage.communityConflict);
      expect(f.admitted, ['one']);
      await f.flow.skipConflictingCommunity();
      expect(f.admitted, ['one', 'three']);
      expect(f.flow.stage, HostedFlowStage.complete);
    },
  );

  test(
    'blocked batch can return to remaining choices and omit failure',
    () async {
      final f = await fixture();
      f.rows = [f.row('one'), f.row('two'), f.row('three')];
      f.beforeAdmission = (name) async {
        if (name == 'two') throw AccountFailure.rejected;
      };
      await f.flow.signInAccount();
      await f.flow.addSelectedCommunities();
      expect(f.flow.canRetry, isFalse);
      f.flow.backToCommunityChoices();
      expect(f.flow.communities.map((item) => item.name), ['two', 'three']);
      f.flow.selectCommunity('wss://two.communities.buzz.xyz', false);
      await f.flow.addSelectedCommunities();
      expect(f.admitted, ['one', 'three']);
    },
  );

  test(
    'single conflicting community can be skipped then creation refreshed',
    () async {
      final f = await fixture();
      f.rows = [f.row('conflict')];
      f.beforeAdmission = (_) async =>
          throw const SavedCommunityIdentityConflict();
      await f.flow.signInAccount();
      await f.flow.skipConflictingCommunity();
      expect(f.flow.stage, HostedFlowStage.chooseCommunities);
      expect(f.flow.communities, isEmpty);
      f.rows = [f.row('created')];
      f.beforeAdmission = null;
      await f.flow.refreshCommunities();
      expect(f.admitted, ['created']);
    },
  );

  test('unsupported rows are skipped and wss prefix canonicalized', () async {
    final f = await fixture();
    f.rows = [
      {...f.row('external'), 'normalized_host': 'external.example'},
      {
        ...f.row('one'),
        'normalized_host': 'wss://one.communities.buzz.xyz:443/',
      },
      f.row('one'),
    ];
    await f.flow.signInAccount();
    expect(f.admitted, ['one']);
  });
}
