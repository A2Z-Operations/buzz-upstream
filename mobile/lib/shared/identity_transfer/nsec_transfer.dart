import 'dart:convert';
import 'dart:io';

import 'package:file_selector/file_selector.dart';
import 'package:flutter/services.dart';
import 'package:flutter/foundation.dart';
import 'package:path_provider/path_provider.dart';
import 'package:share_plus/share_plus.dart';

import '../hosted/account_identity_store.dart';
import '../community/community.dart';
import '../security/sensitive_action_authorizer.dart';

/// Manual transfer boundaries. Exceptions contain no key material.
abstract final class NsecTransfer {
  /// Validates a pasted nsec and returns only its public identity.
  static String preview(String text) {
    if (text.length > 1024) throw AccountIdentityFailure.invalidKey;
    return AccountIdentityStore.publicKeyForNsec(text.trim());
  }

  /// Creates the hosted account slot without replacing an occupied identity.
  static Future<AccountIdentitySnapshot> importAccount(
    String text,
    AccountIdentityStore store, {
    void Function()? checkCurrent,
  }) => store.create(text.trim(), checkCurrent: checkCurrent);

  /// Reads a single small UTF-8 nsec file. Cancellation returns null.
  static Future<String?> pickFile() async {
    if (defaultTargetPlatform == TargetPlatform.android ||
        defaultTargetPlatform == TargetPlatform.iOS) {
      final text = await const MethodChannel(
        'buzz/identity_file',
      ).invokeMethod<String>('import');
      if (text == null) return null;
      preview(text);
      return text.trim();
    }
    final file = await openFile();
    if (file == null) return null;
    return readFile(file);
  }

  /// Validates a small UTF-8 private-key file before returning transient input.
  static Future<String> readFile(XFile file) async {
    try {
      if (await file.length() > 1024) {
        throw AccountIdentityFailure.invalidKey;
      }
      final text = utf8.decode(await file.readAsBytes()).trim();
      preview(text);
      return text;
    } catch (_) {
      throw AccountIdentityFailure.invalidKey;
    }
  }

  /// Authorizes the selected community identity immediately before export.
  static Future<bool> exportCommunity({
    required Community community,
    required Future<DeviceAuthResult> Function(bool biometricOnly) authorize,
    required bool Function() isCurrent,
    required Future<bool> Function(String nsec) share,
  }) async {
    final nsec = community.nsec;
    if (nsec == null || preview(nsec) != community.pubkey) {
      throw AccountIdentityFailure.mismatch;
    }
    final result = await authorize(
      community.sensitiveActionPolicy == SensitiveActionPolicy.enabled,
    );
    if (result != DeviceAuthResult.success || !isCurrent()) return false;
    return share(nsec);
  }

  /// Shares a plaintext backup, removing our temporary file on completion.
  /// The OS share target owns any copy the user chooses to save or send.
  static Future<bool> shareFile(String nsec, Rect origin) async {
    preview(nsec);
    if (Platform.isAndroid) {
      return await const MethodChannel('buzz/identity_file').invokeMethod<bool>(
            'export',
            {'text': '${nsec.trim()}\n', 'name': 'buzz-identity.nsec'},
          ) ??
          false;
    }
    final directory = await (await getTemporaryDirectory()).createTemp(
      'buzz-identity-',
    );
    try {
      final file = File('${directory.path}/buzz-identity.nsec');
      await file.writeAsString('${nsec.trim()}\n', flush: true);
      final result = await SharePlus.instance.share(
        ShareParams(
          files: [XFile(file.path, mimeType: 'text/plain')],
          sharePositionOrigin: origin,
        ),
      );
      return result.status == ShareResultStatus.success;
    } finally {
      await directory.delete(recursive: true);
    }
  }
}
