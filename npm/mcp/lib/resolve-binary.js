"use strict";

/**
 * Resolve a RelayCore native binary from the matching @relay-core/binaries-* optional dependency.
 * Linux uses glibc-linked (gnu) builds so the Deno/V8 script engine is included.
 * @param {string} name - e.g. "relay-core-cli" or "relay-core-probe"
 * @returns {string} Absolute path to the binary
 */
function resolveBinary(name) {
  const platformPkg = `@relay-core/binaries-${process.platform}-${process.arch}`;
  const file = process.platform === "win32" ? `${name}.exe` : name;

  try {
    return require.resolve(`${platformPkg}/bin/${file}`);
  } catch (err) {
    if (process.env.RELAY_CORE_DEBUG) {
      console.error("[relay-core] resolveBinary:", err);
    }
    throw new Error(
      `RelayCore native binary "${name}" is not installed for ${process.platform}-${process.arch}.\n` +
        `Expected optional package ${platformPkg} (install or upgrade from the npm registry).\n` +
        `Supported: macOS (x64, arm64), Linux (x64, arm64), Windows (x64). See https://relaycore.dev`
    );
  }
}

module.exports = { resolveBinary };
