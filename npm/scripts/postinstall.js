#!/usr/bin/env node

const fs = require("node:fs");
const https = require("node:https");
const os = require("node:os");
const path = require("node:path");
const { execFileSync } = require("node:child_process");

const version = require("../package.json").version;
const repo = "baicai-1145/toksqz";

function resolveTarget() {
  const key = `${process.platform}-${process.arch}`;
  const targets = {
    "linux-x64": "x86_64-unknown-linux-musl",
    "linux-arm64": "aarch64-unknown-linux-musl",
    "darwin-x64": "x86_64-apple-darwin",
    "darwin-arm64": "aarch64-apple-darwin"
  };
  return targets[key] || null;
}

function download(url, destination) {
  return new Promise((resolve, reject) => {
    const request = https.get(url, (response) => {
      if (response.statusCode >= 300 && response.statusCode < 400 && response.headers.location) {
        response.resume();
        download(response.headers.location, destination).then(resolve).catch(reject);
        return;
      }
      if (response.statusCode !== 200) {
        reject(new Error(`download failed: ${response.statusCode} ${url}`));
        return;
      }

      const file = fs.createWriteStream(destination);
      response.pipe(file);
      file.on("finish", () => file.close(resolve));
      file.on("error", reject);
    });

    request.on("error", reject);
  });
}

async function main() {
  const target = resolveTarget();
  if (!target) {
    throw new Error(`unsupported platform: ${process.platform} ${process.arch}`);
  }

  const asset = `toksqz-v${version}-${target}.tar.gz`;
  const url = `https://github.com/${repo}/releases/download/v${version}/${asset}`;
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "toksqz-"));
  const archivePath = path.join(tmpDir, asset);
  const extractDir = path.join(tmpDir, "extract");
  const vendorDir = path.join(__dirname, "..", "vendor");
  const binaryName = process.platform === "win32" ? "toksqz.exe" : "toksqz";
  const finalPath = path.join(vendorDir, binaryName);

  fs.mkdirSync(extractDir, { recursive: true });
  fs.mkdirSync(vendorDir, { recursive: true });

  await download(url, archivePath);
  execFileSync("tar", ["-xzf", archivePath, "-C", extractDir], { stdio: "inherit" });

  const extractedPath = path.join(extractDir, "toksqz");
  if (!fs.existsSync(extractedPath)) {
    throw new Error(`archive did not contain toksqz: ${url}`);
  }

  fs.copyFileSync(extractedPath, finalPath);
  fs.chmodSync(finalPath, 0o755);
}

main().catch((error) => {
  console.error(error.message);
  process.exit(1);
});
