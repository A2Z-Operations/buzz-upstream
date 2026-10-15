// Instrumentation-only entry point. Never launches against the normal app ID.
import 'package:flutter/widgets.dart';
import 'package:flutter/services.dart';
import 'package:nostr/nostr.dart' as nostr;

import 'package:buzz/shared/community/community.dart';
import 'package:buzz/shared/community/community_storage.dart';
import 'package:buzz/shared/hosted/account_identity_store.dart';
import 'package:buzz/shared/hosted/builderlab_account.dart';
import 'package:buzz/shared/hosted/device_identity_source.dart';
import 'package:buzz/shared/hosted/hosted_identity_service.dart';
import 'package:buzz/shared/storage/secure_storage.dart';

class _Account extends BuilderlabAccountClient {
  int calls = 0;
  @override
  BuilderlabAccount get account =>
      BuilderlabAccount(subject: 'synthetic', expiresAt: DateTime.utc(2100));
  @override
  Future<Map<String, dynamic>> currentIdentity() async {
    calls++;
    return {};
  }

  @override
  Future<Map<String, dynamic>> post(
    String path,
    Map<String, dynamic> body,
  ) async {
    calls++;
    if (!path.endsWith('/status')) throw StateError('Unexpected remote write');
    return {'backup_nostr_pubkeys': <String>[]};
  }
}

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  const bridge = MethodChannel('buzz/storage_failure_probe');
  // Native test rejects any application ID other than the disposable probe ID.
  if (await bridge.invokeMethod<bool>('isolated') != true) return;
  final account = _Account();
  try {
    final keys = nostr.Keys('1'.padLeft(64, '0'));
    final store = AccountIdentityStore();
    final source = DeviceIdentitySource();
    final resolver = HostedIdentityService(
      account: account,
      store: store,
      deviceIdentities: source,
    );
    // Exercise first-config-wins: a real community writer initializes the plugin
    // before the account store and inventory read through that same namespace.
    await CommunityStorage().saveAll([
      Community.create(
        name: 'Synthetic',
        relayUrl: 'wss://example.invalid',
        nsec: keys.nsec,
        pubkey: keys.public,
      ),
    ]);
    // Only synthetic fixture data in the guarded disposable application.
    await accountSecureStorage.delete(key: AccountIdentityStore.storageKey);
    await communitySecureStorage.write(key: 'probe_sentinel', value: 'keep');
    for (final key in ['buzz_communities', AccountIdentityStore.storageKey]) {
      if (key == AccountIdentityStore.storageKey) {
        await store.create(keys.nsec);
      }
      final beforeCalls = account.calls;
      await bridge.invokeMethod<void>('corrupt', {
        'key': key,
        'options': secureStorageAndroidOptions.toMap(),
      });
      var failedClosed = false;
      try {
        await resolver.resolve();
      } on AccountIdentityFailure catch (failure) {
        failedClosed = failure == AccountIdentityFailure.unreadable;
      }
      if (!failedClosed) throw StateError('Read failure became absence');
      if (key == 'buzz_communities' && await store.read() != null) {
        throw StateError('Generated after unreadable inventory');
      }
      if (key == AccountIdentityStore.storageKey &&
          account.calls != beforeCalls) {
        throw StateError('Unreadable account identity reached remote setup');
      }
      if (await communitySecureStorage.read(key: 'probe_sentinel') != 'keep') {
        throw StateError('Other key lost');
      }
      await bridge.invokeMethod<void>('assertPreserved');
      await bridge.invokeMethod<void>('restore');
    }
    if ((await store.read())?.pubkey != keys.public) {
      throw StateError('Original key did not survive');
    }
    await bridge.invokeMethod<void>('done', {'passed': true});
  } catch (error) {
    // Diagnostic category only; never expose plugin exception details or keys.
    await bridge.invokeMethod<void>('done', {
      'passed': false,
      'error': error.runtimeType.toString(),
    });
  } finally {
    account.dispose();
  }
}
