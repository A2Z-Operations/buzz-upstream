import 'package:buzz/features/onboarding/community_policy.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  test('hosted boundaries and secure ports cannot bypass account policy', () {
    for (final host in [
      'team.communities.buzz.xyz',
      'example.builderlab.xyz',
      'BUZZ.XYZ',
    ]) {
      expect(CommunityPolicy.parse(host)!.hosted, isTrue);
      expect(CommunityPolicy.parse('$host.'), isNull);
      expect(CommunityPolicy.parse('$host:8443'), isNull);
      expect(CommunityPolicy.parse('$host:443')!.hosted, isTrue);
    }
    expect(CommunityPolicy.parse('buzz.xyz.attacker.example')!.hosted, isFalse);
    expect(CommunityPolicy.parse('self.example:8443')!.origin.port, 8443);
    expect(CommunityPolicy.parse('https://user@buzz.xyz'), isNull);
  });
}
