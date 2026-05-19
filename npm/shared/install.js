"use strict";

const { execSync } = require("child_process");
const { existsSync, mkdirSync, readFileSync } = require("fs");
const { createWriteStream } = require("fs");
const { pipeline } = require("stream/promises");
const { join, dirname } = require("path");
const https = require("https");

function getPackageRoot() {
  return dirname(dirname(process.argv[1]));
}

function getVersion() {
  try {
    const pkg = JSON.parse(readFileSync(join(getPackageRoot(), "package.json"), "utf-8"));
    return pkg.version;
  } catch {
    return "0.3.0";
  }
}

function getBinaryName() {
  const name = process.env.npm_package_name || "";
  return name.endsWith("mcp") ? "relay-core-probe" : "relay-core-cli";
}

function getTarget() {
  const os = process.platform;
  const arch = process.arch;
  const map = {
    "darwin-x64": "x86_64-apple-darwin",
    "darwin-arm64": "aarch64-apple-darwin",
    "linux-x64": "x86_64-unknown-linux-gnu",
    "win32-x64": "x86_64-pc-windows-msvc",
  };
  const target = map[`${os}-${arch}`];
  if (!target) {
    console.error(`Unsupported platform: ${os}-${arch}. Supported: macOS (x64/arm64), Linux (x64), Windows (x64).`);
    process.exit(1);
  }
  return target;
}

function getBinaryPath(binaryName) {
  return process.platform === "win32"
    ? join(getPackageRoot(), "bin", binaryName + ".exe")
    : join(getPackageRoot(), "bin", binaryName);
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

module.exports = async function install() {
  try {
    const pkgRoot = getPackageRoot();
    const binDir = join(pkgRoot, "bin");
    mkdirSync(binDir, { recursive: true });
    const binaryName = getBinaryName();
    const binaryPath = getBinaryPath(binaryName);
    const version = getVersion();

    if (existsSync(binaryPath)) {
      try {
        const out = execSync(`"${binaryPath}" --version`, { encoding: "utf-8", timeout: 5000 }).trim();
        if (out.includes(version)) {
          console.log(`${binaryName} v${version} already installed.`);
          return;
        }
        console.log(`Installed ${out}, updating to v${version}...`);
      } catch {
        console.log("Binary exists but failed --version check, re-downloading...");
      }
    }

    const target = getTarget();
    const ext = process.platform === "win32" ? "zip" : "tar.gz";
    const url = `https://github.com/relaycraft/relay-core/releases/download/v${version}/${binaryName}-${target}.${ext}`;
    const archivePath = join(binDir, `${binaryName}.${ext}`);

    await download(url, archivePath);

    console.log("Extracting...");
    if (process.platform === "win32") {
      execSync(`powershell -Command "Expand-Archive -Path '${archivePath}' -DestinationPath '${binDir}' -Force"`, { stdio: "inherit" });
    } else {
      execSync(`tar xzf ${archivePath} -C ${binDir}`, { stdio: "inherit" });
      execSync(`chmod +x ${binaryPath}`, { stdio: "inherit" });
    }

    console.log(`${binaryName} v${version} installed.`);
  } catch (err) {
    console.error("Install failed:", err.message);
    process.exit(1);
  }
};
