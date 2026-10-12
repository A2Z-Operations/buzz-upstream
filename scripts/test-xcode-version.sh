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
printf 'Xcode %s\nBuild version %s\n' "$TEST_XCODE" "${TEST_XCODE_BUILD:-27A266a}"
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
TEST_XCODE=27.0
if TEST_XCODE_BUILD=27A123b check; then echo 'Accepted prerelease build' >&2; exit 1; fi
# Run the real standalone recipe body in an isolated fixture. It must reject
# the compiler before starting cargo or any desktop/device work.
{
  printf "desktop_dir := 'desktop'\nmesh := ''\nfresh := ''\n"
  awk '/^desktop-standalone \*ARGS:/ { active=1; print "desktop-standalone *ARGS:"; next } active && /^$/ { exit } active { print }' "$root/Justfile"
} > "$fixture/Justfile"
cat > "$fixture/bin/cargo" <<'STUB'
#!/usr/bin/env bash
echo 'Native build started before Xcode validation' >&2
exit 99
STUB
chmod +x "$fixture/bin/cargo"
if TEST_XCODE=26.5 "$root/bin/just" --justfile "$fixture/Justfile" desktop-standalone >"$fixture/recipe.log" 2>&1; then
  echo 'Standalone recipe accepted wrong Xcode' >&2; exit 1
fi
grep -q 'Buzz requires Xcode' "$fixture/recipe.log"
if grep -q 'Native build started' "$fixture/recipe.log"; then cat "$fixture/recipe.log"; exit 1; fi
printf 'latest\n' > "$fixture/.xcode-version"
if check; then echo 'Accepted invalid pin' >&2; exit 1; fi
TEST_OS=Linux check
printf 'Xcode version enforcement passed\n'
