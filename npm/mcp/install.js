#!/usr/bin/env node
const { execSync } = require("child_process");
const { existsSync, mkdirSync, readFileSync } = require("fs");
const { createWriteStream } = require("fs");
const { pipeline } = require("stream/promises");
const { join } = require("path");
const https = require("https");

const getVersion = () => {
  try {
    const pkg = JSON.parse(readFileSync(join(__dirname, "package.json"), "utf-8"));
    return pkg.version;
  } catch {
    return "0.3.0";
  }
};

const PACKAGE = process.env.npm_package_name || "@relay-core/cli";
const VERSION = getVersion();

// Map (os, arch) → GitHub release target
function getTarget() {
  const os = process.platform;
  const arch = process.arch;
  const map = {
    "darwin-x64": "x86_64-apple-darwin",
    "darwin-arm64": "aarch64-apple-darwin",
    "linux-x64": "x86_64-unknown-linux-gnu",
  };
  const key = `${os}-${arch}`;
  const target = map[key];
  if (!target) {
    console.error(`Unsupported platform: ${key}. RelayCore currently supports macOS (Intel/Apple Silicon) and Linux (x86_64).`);
    process.exit(1);
  }
  return target;
}

function getBinaryName() {
  return PACKAGE.endsWith("mcp") ? "relay-core-probe" : "relay-core-cli";
}

async function download(url, dest) {
  console.log(`Downloading ${url} → ${dest}`);
  return new Promise((resolve, reject) => {
    const file = createWriteStream(dest);
    https.get(url, (response) => {
      if (response.statusCode === 302 || response.statusCode === 301) {
        https.get(response.headers.location, (redirectRes) => {
          pipeline(redirectRes, file).then(resolve).catch(reject);
        });
        return;
      }
      if (response.statusCode !== 200) {
        reject(new Error(`HTTP ${response.statusCode}`));
        return;
      }
      pipeline(response, file).then(resolve).catch(reject);
    }).on("error", reject);
  });
}

(async () => {
  try {
    const binDir = join(__dirname, "bin");
    const binaryName = getBinaryName();
    const binaryPath = join(binDir, binaryName);

    // Skip download if binary already exists (re-install)
    if (existsSync(binaryPath)) {
      console.log(`${binaryName} already installed.`);
      return;
    }

    const target = getTarget();
    const url = `https://github.com/relaycraft/relay-core/releases/download/v${VERSION}/${binaryName}-${target}.tar.gz`;
    const archivePath = join(binDir, `${binaryName}.tar.gz`);

    mkdirSync(binDir, { recursive: true });
    await download(url, archivePath);

    console.log("Extracting...");
    execSync(`tar xzf ${archivePath} -C ${binDir}`, { stdio: "inherit" });

    // Make executable
    if (process.platform !== "win32") {
      execSync(`chmod +x ${binaryPath}`, { stdio: "inherit" });
    }

    console.log(`${binaryName} v${VERSION} installed.`);
  } catch (err) {
    console.error("Install failed:", err.message);
    process.exit(1);
  }
})();
