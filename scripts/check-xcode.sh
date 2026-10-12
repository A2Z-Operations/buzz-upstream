#!/usr/bin/env bash
# Validate the active Apple toolchain without changing xcode-select globally.
set -euo pipefail
[[ "$(uname -s)" == Darwin ]] || exit 0
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
expected="$(cat "$root/.xcode-version")"
[[ "$expected" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo "Invalid .xcode-version: expected a major.minor.patch version" >&2
  exit 1
}
if ! version="$(xcodebuild -version)"; then
  echo "Buzz requires Xcode $expected. Set DEVELOPER_DIR to its Xcode.app/Contents/Developer." >&2
  exit 1
fi
actual="$(printf '%s\n' "$version" | sed -n 's/^Xcode //p')"
[[ "$actual" == *.* ]] || actual="$actual.0"
[[ "$actual" == *.*.* ]] || actual="$actual.0"
if [[ "$actual" != "$expected" ]]; then
  echo "Buzz requires Xcode $expected (.xcode-version); active toolchain reports: $version" >&2
  echo "Set DEVELOPER_DIR to the matching Xcode.app/Contents/Developer and retry." >&2
  exit 1
fi
printf '%s\n' "$version"
