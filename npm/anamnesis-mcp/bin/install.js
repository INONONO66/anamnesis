#!/usr/bin/env node
"use strict";

const fs = require("fs");
const https = require("https");
const path = require("path");
const { arch, env, platform } = process;
const { execSync } = require("child_process");
const pkg = require("../package.json");

const REPOSITORY = "INONONO66/anamnesis";
const MAX_REDIRECTS = 5;

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

function assetName(key) {
  switch (key) {
    case "darwin-arm64":
      return { file: "anamnesis-darwin-arm64", executable: "anamnesis" };
    case "darwin-x64":
      return { file: "anamnesis-darwin-x64", executable: "anamnesis" };
    case "linux-x64":
      return { file: "anamnesis-linux-x64", executable: "anamnesis" };
    case "linux-arm64":
      return { file: "anamnesis-linux-arm64", executable: "anamnesis" };
    default:
      return null;
  }
}

function download(url, destination, redirects) {
  https
    .get(
      url,
      {
        headers: {
          "User-Agent": "anamnesis-mcp-installer",
        },
      },
      (response) => {
        if (
          response.statusCode >= 300 &&
          response.statusCode < 400 &&
          response.headers.location
        ) {
          response.resume();
          if (redirects >= MAX_REDIRECTS) {
            fail(`too many redirects while downloading ${url}`);
          }
          download(response.headers.location, destination, redirects + 1);
          return;
        }

        if (response.statusCode !== 200) {
          response.resume();
          fail(`download failed (${response.statusCode}) for ${url}`);
        }

        const file = fs.createWriteStream(destination, { mode: 0o755 });
        response.pipe(file);
        file.on("finish", () => {
          file.close(() => {
            if (platform !== "win32") fs.chmodSync(destination, 0o755);
          });
        });
        file.on("error", (err) => {
          fs.rmSync(destination, { force: true });
          fail(err.message);
        });
      }
    )
    .on("error", (err) => fail(err.message));
}

function fail(message) {
  process.stderr.write(`anamnesis postinstall: ${message}\n`);
  process.exit(1);
}

if (env.ANAMNESIS_MCP_SKIP_DOWNLOAD === "1" || env.ANAMNESIS_MCP_BINARY) {
  process.exit(0);
}

const key = platformKey();
const asset = assetName(key);

if (!asset) {
  fail(`no prebuilt binary for ${key}`);
}

const nativeDir = path.join(__dirname, "native");
const destination = path.join(nativeDir, asset.executable);
const tag = `v${pkg.version}`;
const url = `https://github.com/${REPOSITORY}/releases/download/${tag}/${asset.file}`;

fs.mkdirSync(nativeDir, { recursive: true });
download(url, destination, 0);
