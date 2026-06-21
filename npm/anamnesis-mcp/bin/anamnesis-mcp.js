#!/usr/bin/env node
"use strict";
const fs = require("fs");
const path = require("path");
const { arch, env, argv, platform } = process;
const { spawnSync, execSync } = require("child_process");

function isMusl() {
  if (platform !== "linux") return false;
  try {
    return execSync("ldd --version 2>&1", { encoding: "utf8" }).includes("musl");
  } catch (err) {
    return ((err && (err.stdout || err.stderr)) || "").toString().includes("musl");
  }
}

function platformKey() {
  return platform === "linux" && isMusl() ? `linux-${arch}-musl` : `${platform}-${arch}`;
}

function executableName(key) {
  switch (key) {
    case "darwin-arm64":
    case "darwin-x64":
    case "linux-x64":
    case "linux-arm64":
      return "anamnesis-mcp";
    default:
      return null;
  }
}

const key = platformKey();
const executable = executableName(key);
const binaryPath = env.ANAMNESIS_MCP_BINARY || (executable && path.join(__dirname, "native", executable));

if (!binaryPath || !fs.existsSync(binaryPath)) {
  process.stderr.write(
    `anamnesis-mcp: no prebuilt binary for ${key}.\n` +
      `Expected the GitHub Release binary at: ${binaryPath || "(unsupported platform)"}\n` +
      `Try reinstalling the package, or set ANAMNESIS_MCP_BINARY to a local binary.\n` +
      `Supported: darwin-arm64, darwin-x64, linux-x64, linux-arm64\n`
  );
  process.exit(1);
}

const result = spawnSync(binaryPath, argv.slice(2), {
  shell: false,
  stdio: "inherit", // transparent stdin/stdout/stderr — never touch the JSON-RPC stream
  windowsHide: true,
});

if (result.error) {
  const code = result.error.code;
  const hint =
    code === "EACCES" ? " (binary lost its +x bit on publish)" : code === "ENOENT" ? " (binary missing)" : "";
  process.stderr.write(`anamnesis-mcp: ${result.error.message}${hint}\n`);
  process.exit(1);
}
if (result.signal) process.kill(process.pid, result.signal);
else process.exit(result.status == null ? 1 : result.status);
