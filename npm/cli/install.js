#!/usr/bin/env node
"use strict";

const { execSync } = require("child_process");
const { existsSync, mkdirSync, readFileSync, createWriteStream } = require("fs");
const { pipeline } = require("stream/promises");
const { join } = require("path");
const https = require("https");

const PKG_DIR = join(__dirname, "..");
const BINARY_NAME = "relay-core-cli";

function getVersion() {
  try {
    return JSON.parse(readFileSync(join(PKG_DIR, "package.json"), "utf-8")).version;
  } catch { return "0.3.0"; }
}

function getTarget() {
  const map = {
    "darwin-x64": "x86_64-apple-darwin",
    "darwin-arm64": "aarch64-apple-darwin",
    "linux-x64": "x86_64-unknown-linux-gnu",
    "win32-x64": "x86_64-pc-windows-msvc",
  };
  const target = map[`${process.platform}-${process.arch}`];
  if (!target) {
    console.error(`Unsupported platform: ${process.platform}-${process.arch}`);
    process.exit(1);
  }
  return target;
}

async function download(url, dest) {
  console.log(`Downloading ${url} → ${dest}`);
  return new Promise((resolve, reject) => {
    const file = createWriteStream(dest);
    https.get(url, (response) => {
      if (response.statusCode === 302 || response.statusCode === 301) {
        https.get(response.headers.location, (rr) => pipeline(rr, file).then(resolve, reject));
        return;
      }
      if (response.statusCode !== 200) return reject(new Error(`HTTP ${response.statusCode}`));
      pipeline(response, file).then(resolve, reject);
    }).on("error", reject);
  });
}

(async () => {
  try {
    const binDir = join(PKG_DIR, "bin");
    mkdirSync(binDir, { recursive: true });

    const ext = process.platform === "win32" ? ".exe" : "";
    const binaryPath = join(binDir, BINARY_NAME + ext);
    const version = getVersion();

    if (existsSync(binaryPath)) {
      try {
        const out = execSync(`"${binaryPath}" --version`, { encoding: "utf-8", timeout: 5000 }).trim();
        if (out.includes(version)) {
          console.log(`${BINARY_NAME} v${version} already installed.`);
          return;
        }
        console.log(`Installed ${out}, updating to v${version}...`);
      } catch { console.log("Re-downloading..."); }
    }

    const target = getTarget();
    const archiveExt = process.platform === "win32" ? "zip" : "tar.gz";
    const url = `https://github.com/relaycraft/relay-core/releases/download/v${version}/${BINARY_NAME}-${target}.${archiveExt}`;
    const archivePath = join(binDir, `${BINARY_NAME}.${archiveExt}`);

    await download(url, archivePath);
    console.log("Extracting...");

    if (process.platform === "win32") {
      execSync(`powershell -Command "Expand-Archive -Path '${archivePath}' -DestinationPath '${binDir}' -Force"`, { stdio: "inherit" });
    } else {
      execSync(`tar xzf ${archivePath} -C ${binDir}`, { stdio: "inherit" });
      execSync(`chmod +x ${binaryPath}`, { stdio: "inherit" });
    }
    console.log(`${BINARY_NAME} v${version} installed.`);
  } catch (err) {
    console.error("Install failed:", err.message);
    process.exit(1);
  }
})();
