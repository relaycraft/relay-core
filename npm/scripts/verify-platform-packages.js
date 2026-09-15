#!/usr/bin/env node
"use strict";

/**
 * Ensure every @relay-core/binaries-* package for this version exists on the npm registry.
 * Exits non-zero if any are missing (blocks incomplete wrapper releases).
 *
 * The check polls rather than asking once. npm's read path lags behind a successful publish, so a
 * single read right after the publish-binaries matrix finishes can report platforms as missing when
 * they are in fact published — which happened on the v0.11.0 release, blocked the wrappers, and
 * needed a manual re-run to clear. Waiting distinguishes "not visible yet" from "not published",
 * which is the difference between a false alarm and a real incomplete release.
 *
 * Usage: node npm/scripts/verify-platform-packages.js 0.3.9
 *        node npm/scripts/verify-platform-packages.js v0.3.9
 *        VERIFY_TIMEOUT_MS=30000 node npm/scripts/verify-platform-packages.js 0.3.9
 */
const { execSync } = require("child_process");

const PLATFORMS = [
  "darwin-arm64",
  "darwin-x64",
  "linux-arm64",
  "linux-x64",
  "win32-x64",
];

/** How long to keep waiting for the registry to catch up. Override with VERIFY_TIMEOUT_MS. */
const TIMEOUT_MS = Number(process.env.VERIFY_TIMEOUT_MS || 180_000);
/** Gap between attempts. */
const INTERVAL_MS = Number(process.env.VERIFY_INTERVAL_MS || 5_000);

const raw = process.argv[2];
if (!raw) {
  console.error("Usage: node npm/scripts/verify-platform-packages.js <version>");
  process.exit(1);
}

const version = raw.replace(/^v/, "");

/** Is this exact version published? Any failure to read counts as "not yet". */
function isPublished(platform) {
  const spec = `@relay-core/binaries-${platform}@${version}`;
  try {
    const published = execSync(`npm view "${spec}" version`, {
      encoding: "utf-8",
      stdio: ["ignore", "pipe", "pipe"],
    }).trim();
    return { ok: published === version, published };
  } catch {
    return { ok: false, published: null };
  }
}

function sleep(ms) {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

function allPublished() {
  const missing = [];
  for (const platform of PLATFORMS) {
    const { ok, published } = isPublished(platform);
    if (ok) {
      console.log(`  ok @relay-core/binaries-${platform}@${version}`);
    } else {
      missing.push(
        published === null
          ? `@relay-core/binaries-${platform}@${version}`
          : `@relay-core/binaries-${platform}@${version} (registry reports ${published})`
      );
    }
  }
  return missing;
}

console.log(`Verifying ${PLATFORMS.length} platform packages @ ${version} on npm registry...`);

const deadline = Date.now() + TIMEOUT_MS;
let missing = allPublished();
let attempt = 1;

while (missing.length > 0 && Date.now() < deadline) {
  const remaining = Math.round((deadline - Date.now()) / 1000);
  console.log(
    `\n${missing.length} not visible yet (attempt ${attempt}); ` +
      `npm's read path lags behind a publish. Retrying in ${INTERVAL_MS / 1000}s (${remaining}s left)...`
  );
  sleep(INTERVAL_MS);
  attempt += 1;
  missing = allPublished();
}

if (missing.length > 0) {
  console.error(
    `\nIncomplete platform release — still missing after ${Math.round(TIMEOUT_MS / 1000)}s ` +
      `and ${attempt} attempt(s); refusing to publish wrappers:`
  );
  for (const m of missing) console.error(`  missing: ${m}`);
  console.error(
    "\nFix: re-run the failed publish-binaries matrix jobs, or bump to a new version " +
      "(npm does not allow republishing the same version)."
  );
  process.exit(1);
}

console.log(`\nAll platform packages present${attempt > 1 ? ` (after ${attempt} attempts)` : ""}.`);
