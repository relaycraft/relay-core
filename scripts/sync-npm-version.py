"""Sync npm package.json versions with new workspace version.
Called as a cargo-release pre-release-hook with NEW_VERSION env var."""
import json, os, sys

version = os.environ.get("NEW_VERSION")
if not version:
    print("NEW_VERSION not set, skipping npm sync", file=sys.stderr)
    sys.exit(0)

for pkg in ["npm/cli/package.json", "npm/mcp/package.json"]:
    with open(pkg) as f:
        data = json.load(f)
    old = data["version"]
    data["version"] = version
    with open(pkg, "w") as f:
        json.dump(data, f, indent=2)
        f.write("\n")
    print(f"  {pkg}: {old} → {version}")
