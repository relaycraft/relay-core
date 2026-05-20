#!/usr/bin/env node
"use strict";

/**
 * Sync npm package versions from a release tag or semver string.
 * Usage: node npm/scripts/set-version.js 0.3.9
 *        node npm/scripts/set-version.js v0.3.9
 */
const { existsSync, readFileSync, writeFileSync, readdirSync, statSync } = require("fs");
const { join } = require("path");

const ROOT = join(__dirname, "..");
const raw = process.argv[2];
if (!raw) {
  console.error("Usage: node npm/scripts/set-version.js <version>");
  process.exit(1);
}

const version = raw.replace(/^v/, "");

function setPkgVersion(filePath) {
  const pkg = JSON.parse(readFileSync(filePath, "utf-8"));
  pkg.version = version;
  if (pkg.optionalDependencies) {
    for (const name of Object.keys(pkg.optionalDependencies)) {
      if (name.startsWith("@relay-core/binaries-")) {
        pkg.optionalDependencies[name] = version;
      }
    }
  }
  writeFileSync(filePath, JSON.stringify(pkg, null, 2) + "\n");
  console.log(`  ${pkg.name} → ${version}`);
}

console.log(`Setting npm packages to ${version}...`);

for (const name of readdirSync(join(ROOT, "packages"))) {
  const dir = join(ROOT, "packages", name);
  if (!statSync(dir).isDirectory()) continue;
  const pkgPath = join(dir, "package.json");
  if (!existsSync(pkgPath)) {
    console.warn(`  skip ${name}: no package.json`);
    continue;
  }
  setPkgVersion(pkgPath);
}

setPkgVersion(join(ROOT, "cli", "package.json"));
setPkgVersion(join(ROOT, "mcp", "package.json"));

console.log("Done.");
