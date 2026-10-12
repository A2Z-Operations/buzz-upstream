#!/usr/bin/env bash
# Exercise version enforcement without installing or switching a real Xcode.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT
mkdir -p "$fixture/scripts" "$fixture/bin"
cp "$root/scripts/check-xcode.sh" "$fixture/scripts/"
cp "$root/.xcode-version" "$fixture/"
cat > "$fixture/bin/uname" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "${TEST_OS:-Darwin}"
STUB
cat > "$fixture/bin/xcodebuild" <<'STUB'
#!/usr/bin/env bash
[[ "${TEST_XCODE:-}" != broken ]] || exit 1
printf 'Xcode %s\nBuild version fixture\n' "$TEST_XCODE"
STUB
chmod +x "$fixture/bin/"*
export PATH="$fixture/bin:$PATH"
check() { "$fixture/scripts/check-xcode.sh" >/dev/null 2>&1; }
export TEST_XCODE=27.0
check
export TEST_XCODE=27
check
for TEST_XCODE in 26.6 27.1 '27.0 beta' broken ''; do
  export TEST_XCODE
  if check; then echo "Accepted wrong Xcode: $TEST_XCODE" >&2; exit 1; fi
done
printf 'latest\n' > "$fixture/.xcode-version"
if check; then echo 'Accepted invalid pin' >&2; exit 1; fi
TEST_OS=Linux check
printf 'Xcode version enforcement passed\n'
