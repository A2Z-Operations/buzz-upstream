require 'minitest/autorun'
require 'tmpdir'
require_relative '../mobile/ios/trust_swift_plugins'

class SwiftPluginTrustTest < Minitest::Test
  def setup
    @root = Dir.mktmpdir
    @directory = File.join(@root, 'security')
    @lockfile = File.join(@root, 'Package.resolved')
    @pin = {
      'identity' => 'swift-secp256k1',
      'location' => BuzzSwiftPluginTrust::PACKAGE_URL,
      'state' => { 'revision' => BuzzSwiftPluginTrust::APPROVAL.fetch('fingerprint') },
    }
    write_pin
  end

  def teardown
    FileUtils.remove_entry(@root)
  end

  def write_pin
    File.write(@lockfile, JSON.generate('pins' => [@pin]))
  end

  def install
    BuzzSwiftPluginTrust.install([@lockfile], @directory)
  end

  def test_approves_only_the_pinned_plugin_and_is_idempotent
    install
    path = File.join(@directory, 'plugins.json')
    assert_equal [BuzzSwiftPluginTrust::APPROVAL], JSON.parse(File.read(path))
    before = File.read(path)
    install
    assert_equal before, File.read(path)
  end

  def test_preserves_other_project_approvals
    FileUtils.mkdir_p(@directory)
    other = { 'fingerprint' => 'other', 'packageIdentity' => 'other', 'targetName' => 'OtherPlugin' }
    File.write(File.join(@directory, 'plugins.json'), JSON.generate([other]))
    install
    assert_equal [other, BuzzSwiftPluginTrust::APPROVAL], JSON.parse(File.read(File.join(@directory, 'plugins.json')))
  end

  def test_rejects_changed_revision_before_writing_trust
    @pin['state']['revision'] = 'unreviewed'
    write_pin
    assert_raises(RuntimeError) { install }
    refute File.exist?(@directory)
  end

  def test_rejects_another_repository_with_the_same_package_name
    @pin['location'] = 'https://example.com/swift-secp256k1.git'
    write_pin
    assert_raises(RuntimeError) { install }
    refute File.exist?(@directory)
  end

  def test_does_not_overwrite_malformed_existing_trust
    FileUtils.mkdir_p(@directory)
    path = File.join(@directory, 'plugins.json')
    File.write(path, '{broken')
    assert_raises(JSON::ParserError) { install }
    assert_equal '{broken', File.read(path)
  end
end
