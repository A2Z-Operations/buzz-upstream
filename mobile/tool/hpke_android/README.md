# Android HPKE verification

Activate the repository Hermit environment and use a Gradle-supported JDK (21).
The Android SDK and Flutter dependencies must already be installed.

```sh
mobile/android/gradlew -p mobile/android :app:testDebugUnitTest \
  --tests 'xyz.block.buzz.mobile.HostedHpke*Test'
python3 mobile/tool/hpke_android/check_interop.py
```

The focused JVM tests cover the published RFC 9180 P-256/HKDF-SHA256/AES-256-GCM
base-mode known-answer vector, point/input rejection, fresh encapsulation,
registered Flutter codec dispatch, plaintext buffer clearing and redacted errors.

The macOS interoperability check invokes the **actual compiled Android sender**
with the repository's public test-only backend fixture. Apple CryptoKit decrypts
its output with the fixture recipient key; changed ciphertext and AAD must fail.
`--classes PATH` selects separately compiled classes, including isolated mutation
builds. Never supply real identities or backups to these verification tools.

These checks do not prove the full hosted network flow, Android document-picker
UI, or Android platform-provider behavior; those need device workflow evidence.
