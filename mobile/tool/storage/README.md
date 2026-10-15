# Android secure-storage failure probe

This instrumentation test uses the pinned `flutter_secure_storage` plugin,
Android Keystore, and the production community/account identity services. It
corrupts the authentication tag of synthetic encrypted community and account
records. The resolver must report unreadable storage, preserve the damaged
ciphertext and other keys, and never generate an identity.

Use an isolated checkout with its own build outputs and an emulator. In that
checkout, set the **gitignored** `android/AppOverrides.properties` to:

```properties
applicationIdSuffix=.identitysafety
appName=Buzz Identity Safety Probe
```

From `mobile/android`, with the repository Flutter toolchain and JDK 21:

```sh
ANDROID_SERIAL=emulator-5554 ./gradlew :app:connectedDebugAndroidTest \
  -Ptarget="$PWD/../tool/storage/android_read_failure_probe.dart" \
  -Pandroid.testInstrumentationRunnerArguments.class=xyz.block.buzz.mobile.SecureStorageReadFailureTest
```

Verify the emulator serial with `adb devices` first. The test must report one
executed test, zero failures and **zero skipped**. It intentionally skips normal
app IDs; do not treat that skip as a pass. The native bridge checks the disposable
app ID before starting Dart, and the Dart entry point requires that bridge before
writing anything. No live account/backend is used. Normal app entry points and
release builds do not use this probe.

The first storage access is through `CommunityStorage`, so hosted reads reuse its
cached plugin configuration. Both storage constant variants share fail-closed
Android options while retaining their existing iOS settings. Regression control:
in an isolated source/build tree, changing the shared `resetOnError` option to
`true` must fail the test. Restore the source and rerun the clean control.
