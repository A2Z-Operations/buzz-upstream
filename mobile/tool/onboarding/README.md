# Onboarding preview and interoperability check

The optional preview runs onboarding widgets with memory-only services. It does
not connect to a community or read or write real credentials. The production
entry point remains `lib/main.dart`.

From the repository root:

```sh
. ./bin/activate-hermit
cd mobile
flutter run -d <simulator-id> -t tool/onboarding/main.dart
```

Use `example.communities.buzz.xyz` for a hosted community or
`community.example.org` for a self-hosted community. Pairing accepts only
`demo-hosted` and `demo-self-hosted`; key import accepts only `demo-key`.
Never enter a real private key into this preview.

The scenario selector covers new and recovered identities, existing keys,
mismatches, unavailable recovery, backup failure, and denied admission. These
fixtures exercise navigation and coordinator behavior, not live service readiness.

Production integration lives in `lib/shared/hosted/` and `lib/hosted_app_gate.dart`.
Account sign-in uses production by default; public staging can be selected with
`--dart-define=BUZZ_BUILDERLAB_URL=https://app.builderlabstaging.xyz`.
Staging community routing and full community-entry acceptance remain incomplete.
Android has no native HPKE implementation yet; iOS sealing requires iOS 17+.

`hpke_interop.swift`, compiled with `ios/Runner/HostedHpkeBackup.swift`, checks
native sealing against the existing synthetic fixture at
`../desktop/src-tauri/src/testdata/hpke_nsec_backup_v1.json` (relative to `mobile/`).
The fixture keys are test data and must never be used for a real identity.
