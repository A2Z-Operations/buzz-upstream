/// Bundled classification for the preview. Production issuer/session contracts
/// must be configured separately; classification never trusts a relay's issuer.
class CommunityPolicy {
  const CommunityPolicy(this.origin, this.hosted);
  final Uri origin;
  final bool hosted;

  static CommunityPolicy? parse(String input) {
    final raw = input.trim();
    final uri = Uri.tryParse(raw.contains('://') ? raw : 'https://$raw');
    if (uri == null ||
        !['https', 'wss'].contains(uri.scheme) ||
        uri.host.isEmpty ||
        uri.host.endsWith('.') ||
        uri.userInfo.isNotEmpty ||
        uri.hasQuery ||
        uri.hasFragment ||
        (uri.path.isNotEmpty && uri.path != '/')) {
      return null;
    }
    final host = uri.host.toLowerCase();
    // TODO: Confirm production Builderlab domain scope with the owner.
    // This broad classification applies only to the simulated preview.
    final hosted = [
      'buzz.xyz',
      'builderlab.xyz',
    ].any((domain) => host == domain || host.endsWith('.$domain'));
    // An alternate hosted port needs an explicit policy entry. Never downgrade
    // it to self-hosted. Self-hosted deployments retain custom secure ports.
    if (hosted && uri.hasPort && uri.port != 443) return null;
    return CommunityPolicy(
      Uri(
        scheme: 'https',
        host: host,
        port: uri.hasPort && uri.port != 443 ? uri.port : null,
      ),
      hosted,
    );
  }
}
