#!/usr/bin/env python3
"""After :app:compileDebugKotlin, decrypt the Android output using Apple HPKE.

Requires macOS Swift/CryptoKit, Java 17+, and the Gradle-resolved Kotlin stdlib.
Only the repository's public test fixture is used. All artifacts are temporary.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[3]
cache = Path(os.environ.get("GRADLE_USER_HOME", Path.home() / ".gradle"))
stdlib = sorted((cache / "caches/modules-2/files-2.1/org.jetbrains.kotlin/kotlin-stdlib").glob("*/*/kotlin-stdlib-[0-9]*.jar"))
stdlib = [p for p in stdlib if not p.name.endswith(("-sources.jar", "-javadoc.jar"))][-1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--classes", type=Path, default=root / "mobile/build/app/tmp/kotlin-classes/debug")
classes = parser.parse_args().classes
fixture = json.loads((root / "desktop/src-tauri/src/testdata/hpke_nsec_backup_v1.json").read_text())
with tempfile.TemporaryDirectory(prefix="buzz-android-hpke-") as temp:
    classpath = os.pathsep.join(map(str, (classes, stdlib, temp)))
    subprocess.run(["javac", "--release", "17", "-cp", classpath, "-d", temp, str(Path(__file__).with_name("Seal.java"))], check=True)
    output = subprocess.check_output([
        "java", "-cp", classpath, "Seal", fixture["recipient_public_key_sec1_hex"],
        fixture["nostr_secret_key_hex"], fixture["aad_hex"],
    ], text=True).splitlines()
    fixture["envelope"].update(enc=output[0], ciphertext=output[1])
    sealed = Path(temp) / "android.json"
    sealed.write_text(json.dumps(fixture))
    executable = str(Path(temp) / "interop")
    subprocess.run([
        "swiftc", str(root / "mobile/ios/Runner/HostedHpkeBackup.swift"),
        str(root / "mobile/tool/onboarding/hpke_interop.swift"), "-o", executable,
    ], check=True)
    subprocess.run([executable, str(sealed)], check=True)
    # Authentication must reject changes to the actual Android ciphertext and AAD.
    for field in ("aad_hex", "ciphertext"):
        changed = json.loads(sealed.read_text())
        if field == "aad_hex":
            changed[field] = "00" + changed[field][2:]
        else:
            cipher = changed["envelope"][field]
            changed["envelope"][field] = ("B" if cipher[0] == "A" else "A") + cipher[1:]
        bad = Path(temp) / "tampered.json"
        bad.write_text(json.dumps(changed))
        result = subprocess.run([executable, str(bad)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if result.returncode == 0:
            raise RuntimeError(f"Accepted modified {field}")
    print("Android sender → CryptoKit receiver: decrypt and tamper rejection passed")
