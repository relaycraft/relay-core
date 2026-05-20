"""Sync all npm package.json versions with the workspace release version.

Called as a cargo-release pre-release-hook with NEW_VERSION env var.
Delegates to npm/scripts/set-version.js (cli, mcp, and every @relay-core/binaries-* package).
"""
import os
import subprocess
import sys

version = os.environ.get("NEW_VERSION")
if not version:
    print("NEW_VERSION not set, skipping npm sync", file=sys.stderr)
    sys.exit(0)

repo_root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
script = os.path.join(repo_root, "npm", "scripts", "set-version.js")

print(f"Syncing npm packages to {version}...")
subprocess.run(["node", script, version], cwd=repo_root, check=True)
