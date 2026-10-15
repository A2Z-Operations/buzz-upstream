/// Trusted build configuration, never supplied by a deep link or relay.
abstract final class HostedEnvironment {
  static final Uri origin = _origin();

  static Uri _origin() {
    const configured = String.fromEnvironment(
      'BUZZ_BUILDERLAB_URL',
      defaultValue: 'https://app.builderlab.xyz',
    );
    final uri = Uri.parse(configured);
    if (!{
      'https://app.builderlab.xyz',
      'https://app.builderlabstaging.xyz',
    }.contains(configured)) {
      throw StateError('Unsupported hosted account environment');
    }
    return uri;
  }
}
