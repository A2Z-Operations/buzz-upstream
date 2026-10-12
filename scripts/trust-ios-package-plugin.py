#!/usr/bin/env python3
"""Trust only Buzz's reviewed, pinned Swift build plugin on a build worker."""
import argparse
import json
from pathlib import Path


def trust_plugin(destination, root):
    """Preserve other trust entries and reject an unreviewed package revision."""
    entry = {
        "fingerprint": "e70a10e036a55fffea31568f0af92d69b6d449cd",
        "packageIdentity": "swift-secp256k1",
        "targetName": "SharedSourcesPlugin",
    }
    lock = json.loads((root / "mobile/ios/Runner.xcworkspace/xcshareddata/swiftpm/Package.resolved").read_text())
    pin = next(pin for pin in lock["pins"] if pin["identity"] == entry["packageIdentity"])
    if pin["state"]["revision"] != entry["fingerprint"]:
        raise ValueError("Review the changed Swift plugin before updating its trusted fingerprint")
    entries = json.loads(destination.read_text()) if destination.exists() else []
    if not isinstance(entries, list):
        raise ValueError("Expected a Swift plugin trust list")
    if entry not in entries:
        entries.append(entry)
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text(json.dumps(entries, indent=2) + "\n")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--trust-file", type=Path, required=True)
    args = parser.parse_args()
    trust_plugin(args.trust_file, Path(__file__).resolve().parent.parent)
