require 'json'
require 'fileutils'
require 'tempfile'

# Xcode records approval per plugin and package revision. Never disable plugin
# validation globally: a dependency update must explicitly update this approval.
module BuzzSwiftPluginTrust
  APPROVAL = {
    'fingerprint' => 'e70a10e036a55fffea31568f0af92d69b6d449cd',
    'packageIdentity' => 'swift-secp256k1',
    'targetName' => 'SharedSourcesPlugin',
  }.freeze
  PACKAGE_URL = 'https://github.com/21-DOT-DEV/swift-secp256k1.git'.freeze

  def self.install(lockfiles, directory)
    lockfiles.each do |lockfile|
      pins = JSON.parse(File.read(lockfile)).fetch('pins')
      pin = pins.find { |entry| entry['identity'] == APPROVAL.fetch('packageIdentity') }
      unless pin && pin['location'] == PACKAGE_URL &&
          pin.fetch('state').fetch('revision') == APPROVAL.fetch('fingerprint')
        raise "Unapproved swift-secp256k1 revision in #{lockfile}; review the plugin before updating its approval"
      end
    end

    FileUtils.mkdir_p(directory)
    path = File.join(directory, 'plugins.json')
    File.open(File.join(directory, 'buzz-plugins.lock'), File::RDWR | File::CREAT, 0o600) do |lock|
      lock.flock(File::LOCK_EX)
      approvals = File.exist?(path) ? JSON.parse(File.read(path)) : []
      raise "Expected an array in #{path}" unless approvals.is_a?(Array)
      return if approvals.include?(APPROVAL)

      # Preserve approvals for other projects and publish a complete JSON file.
      Tempfile.create(['plugins', '.json'], directory) do |file|
        file.write(JSON.pretty_generate(approvals + [APPROVAL]) + "\n")
        file.flush
        File.rename(file.path, path)
      end
    end
  end
end
