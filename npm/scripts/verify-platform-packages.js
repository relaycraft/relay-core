#!/usr/bin/env node
"use strict";

/**
 * Ensure every @relay-core/binaries-* package for this version exists on the npm registry.
 * Exits non-zero if any are missing (blocks incomplete wrapper releases).
 *
 * Usage: node npm/scripts/verify-platform-packages.js 0.3.9
 *        node npm/scripts/verify-platform-packages.js v0.3.9
 */
const { execSync } = require("child_process");

const PLATFORMS = [
  "darwin-arm64",
  "darwin-x64",
  "linux-arm64",
  "linux-x64",
  "win32-x64",
];

const raw = process.argv[2];
if (!raw) {
  console.error("Usage: node npm/scripts/verify-platform-packages.js <version>");
  process.exit(1);
}

const version = raw.replace(/^v/, "");
const missing = [];

console.log(`Verifying ${PLATFORMS.length} platform packages @ ${version} on npm registry...`);

for (const platform of PLATFORMS) {
  const name = `@relay-core/binaries-${platform}`;
  const spec = `${name}@${version}`;
  try {
    const published = execSync(`npm view "${spec}" version`, {
      encoding: "utf-8",
      stdio: ["ignore", "pipe", "pipe"],
    }).trim();
    if (published !== version) {
      missing.push(`${spec} (got version ${published})`);
      continue;
    }
    console.log(`  ok ${spec}`);
  } catch {
    missing.push(spec);
  }
}

if (missing.length > 0) {
  console.error("\nIncomplete platform release — refusing to publish wrappers:");
  for (const m of missing) console.error(`  missing: ${m}`);
  console.error(
    "\nFix: re-run failed publish-binaries matrix jobs, or bump to a new version " +
      "(npm does not allow republishing the same version)."
  );
  process.exit(1);
}

console.log("All platform packages present.");
