#!/usr/bin/env node
"use strict";
const { platform, arch, env, argv } = process;
const { spawnSync, execSync } = require("child_process");

function isMusl() {
  if (platform !== "linux") return false;
  try {
    return execSync("ldd --version 2>&1", { encoding: "utf8" }).includes("musl");
  } catch (err) {
    return ((err && (err.stdout || err.stderr)) || "").toString().includes("musl");
  }
}

const PLATFORMS = {
  "darwin-arm64": "@anamnesis/mcp-darwin-arm64/anamnesis-mcp",
  "darwin-x64": "@anamnesis/mcp-darwin-x64/anamnesis-mcp",
  "linux-x64": "@anamnesis/mcp-linux-x64/anamnesis-mcp",
  "linux-arm64": "@anamnesis/mcp-linux-arm64/anamnesis-mcp",
  "win32-x64": "@anamnesis/mcp-win32-x64/anamnesis-mcp.exe",
};

const key =
  platform === "linux" && isMusl() ? `linux-${arch}-musl` : `${platform}-${arch}`;
const subpath = env.ANAMNESIS_MCP_BINARY ? null : PLATFORMS[key];

let binaryPath;
try {
  binaryPath = env.ANAMNESIS_MCP_BINARY || require.resolve(subpath);
} catch (e) {
  process.stderr.write(
    `anamnesis-mcp: no prebuilt binary for ${key}.\n` +
      `The matching optional dependency was not installed (a known npm bug can prune it).\n` +
      `Try: rm -rf node_modules package-lock.json && npm install\n` +
      `Supported: ${Object.keys(PLATFORMS).join(", ")}\n`
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
