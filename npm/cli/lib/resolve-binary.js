"use strict";

/**
 * Resolve a RelayCore native binary from the matching @relay-core/binaries-* optional dependency.
 * On Linux, prefers the musl (statically linked) variant for maximum distro compatibility.
 * @param {string} name - e.g. "relay-core-cli" or "relay-core-probe"
 * @returns {string} Absolute path to the binary
 */
function resolveBinary(name) {
  const platformPkg = `@relay-core/binaries-${process.platform}-${process.arch}`;
  const file = process.platform === "win32" ? `${name}.exe` : name;

  if (process.platform === "linux") {
    const muslPkg = `${platformPkg}-musl`;
    try {
      return require.resolve(`${muslPkg}/bin/${file}`);
    } catch {
      // musl package not installed — fall through to gnu
    }
  }

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
