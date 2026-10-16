import '../deeplink/deep_link.dart';
import 'builderlab_account.dart';
import 'hosted_environment.dart';

/// Validated, active communities owned by the signed-in Builderlab identity.
/// Discovery never grants admission; each target still goes through HostedAdmission.
Future<List<AddCommunityDeepLink>> loadHostedCommunities(
  BuilderlabAccountClient account,
) async {
  final response = await account.post('/v1/buzz/communities/list', {});
  final rows = response['communities'];
  if (response['error'] != null || rows is! List) {
    throw AccountFailure.invalidResponse;
  }
  final communities = <String, AddCommunityDeepLink>{};
  for (final row in rows) {
    if (row is! Map<String, dynamic>) throw AccountFailure.invalidResponse;
    final archived = row['archived_at'];
    if (archived != null) {
      if (archived is! String || archived.isEmpty) {
        throw AccountFailure.invalidResponse;
      }
      continue;
    }
    final host = row['normalized_host'];
    final name = row['name'];
    if (host is! String ||
        !RegExp(r'^[a-z0-9]+(?:[.-][a-z0-9]+)*$').hasMatch(host) ||
        name is! String ||
        name.trim().isEmpty) {
      throw AccountFailure.invalidResponse;
    }
    final relay = Uri(scheme: 'wss', host: host);
    if (!HostedEnvironment.supportsRelay(relay)) {
      throw AccountFailure.invalidResponse;
    }
    communities.putIfAbsent(
      host,
      () => AddCommunityDeepLink(relayUrl: relay.toString(), name: name),
    );
  }
  return List.unmodifiable(communities.values);
}
