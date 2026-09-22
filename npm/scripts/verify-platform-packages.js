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
 *        node npm/scripts/verify-platform-packages.js 0.3.9 linux-arm64
 *        VERIFY_TIMEOUT_MS=30000 node npm/scripts/verify-platform-packages.js 0.3.9
 *
 * A platform argument waits for that one package. The publish matrix uses it so a job stays
 * incomplete until the abbreviated packument lists the version it just uploaded.
 */


const PLATFORMS = [
  "darwin-arm64",
  "darwin-x64",
  "linux-arm64",
  "linux-x64",
  "win32-x64",
];

/**
 * How long to keep waiting for the registry to catch up. Override with VERIFY_TIMEOUT_MS.
 *
 * 15 minutes. On v0.13.1 the linux-arm64 tarball (about 47MB) was accepted at once and stayed
 * off this abbreviated document for about 10 minutes, past the previous 180s budget.
 */
const TIMEOUT_MS = Number(process.env.VERIFY_TIMEOUT_MS || 15 * 60 * 1000);
/** Gap between attempts. */
const INTERVAL_MS = Number(process.env.VERIFY_INTERVAL_MS || 5_000);

const raw = process.argv[2];
if (!raw) {
  console.error("Usage: node npm/scripts/verify-platform-packages.js <version>");
  process.exit(1);
}

const version = raw.replace(/^v/, "");
const onlyPlatform = process.argv[3];
if (onlyPlatform && !PLATFORMS.includes(onlyPlatform)) {
  console.error(`Unknown platform "${onlyPlatform}". Expected one of: ${PLATFORMS.join(", ")}`);
  process.exit(1);
}
const platforms = onlyPlatform ? [onlyPlatform] : PLATFORMS;

/**
 * Is this exact version published?
 *
 * Asked of the registry directly, with the *abbreviated* packument. `npm view` fetches the full
 * document, and on the v0.12.0 release that one lagged: the wrapper check polled for three minutes
 * and never saw `binaries-linux-arm64@0.12.0`, while the abbreviated document already listed it. The
 * publish job had succeeded; only the reader was behind. Fetching the smaller document is both
 * fresher and far cheaper than shelling out to npm for each attempt.
 */
async function isPublished(platform) {
  const name = `@relay-core/binaries-${platform}`;
  try {
    const response = await fetch(`https://registry.npmjs.org/${encodeURIComponent(name)}`, {
      headers: { Accept: "application/vnd.npm.install-v1+json" },
    });
    if (!response.ok) return { ok: false, published: null };
    const doc = await response.json();
    const versions = Object.keys(doc.versions || {});
    // Sorted by version, not by string: `0.12.0` sorts before `0.9.5`, which is how an earlier check
    // of this release was misread by hand.
    const has = versions.includes(version);
    return { ok: has, published: has ? version : versions.slice(-1)[0] ?? null };
  } catch {
    return { ok: false, published: null };
  }
}

function sleep(ms) {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

async function allPublished() {
  const missing = [];
  for (const platform of platforms) {
    const { ok, published } = await isPublished(platform);
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

console.log(`Verifying ${platforms.length} platform package(s) @ ${version} on npm registry...`);

const deadline = Date.now() + TIMEOUT_MS;
let missing = await allPublished();
let attempt = 1;

while (missing.length > 0 && Date.now() < deadline) {
  const remaining = Math.round((deadline - Date.now()) / 1000);
  console.log(
    `\n${missing.length} not visible yet (attempt ${attempt}); ` +
      `npm's read path lags behind a publish. Retrying in ${INTERVAL_MS / 1000}s (${remaining}s left)...`
  );
  sleep(INTERVAL_MS);
  attempt += 1;
  missing = await allPublished();
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
