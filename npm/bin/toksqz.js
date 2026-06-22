#!/usr/bin/env node

const { spawnSync } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");

const binName = process.platform === "win32" ? "toksqz.exe" : "toksqz";
const binaryPath = path.join(__dirname, "..", "vendor", binName);

if (!fs.existsSync(binaryPath)) {
  console.error("toksqz binary is missing. Reinstall the package to download it.");
  process.exit(1);
}

const result = spawnSync(binaryPath, process.argv.slice(2), {
  stdio: "inherit",
  env: process.env
});

if (result.error) {
  console.error(result.error.message);
  process.exit(1);
}

process.exit(result.status === null ? 1 : result.status);
