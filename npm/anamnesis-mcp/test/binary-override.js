"use strict";

const assert = require("assert");
const fs = require("fs");
const os = require("os");
const path = require("path");
const { spawnSync } = require("child_process");

const packageRoot = path.resolve(__dirname, "..");
const installer = path.join(packageRoot, "bin", "install.js");
const launcher = path.join(packageRoot, "bin", "anamnesis.js");
const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "anamnesis-npm-override-"));
const baseEnv = { ...process.env };
for (const name of ["ANAMNESIS_MCP_BINARY", "ANAMNESIS_BINARY", "ANAMNESIS_MCP_SKIP_DOWNLOAD", "NODE_OPTIONS"]) {
  delete baseEnv[name];
}

const blockDownload = path.join(tempDir, "block-download.js");
fs.writeFileSync(
  blockDownload,
  `"use strict";\nrequire("https").get = () => {\n  process.stderr.write("unexpected download\\n");\n  process.exit(99);\n};\n`
);

function fakeBinary(name) {
  const file = path.join(tempDir, name);
  fs.writeFileSync(file, `#!/bin/sh\nprintf '${name}:%s\\n' "$*"\nexit 0\n`, { mode: 0o755 });
  return file;
}

function runNode(script, args, extraEnv) {
  return spawnSync(process.execPath, [script, ...args], {
    cwd: packageRoot,
    env: { ...baseEnv, ...extraEnv },
    encoding: "utf8",
  });
}

try {
  const canonical = fakeBinary("canonical");
  const legacy = fakeBinary("legacy");

  for (const [name, value] of [
    ["ANAMNESIS_MCP_BINARY", canonical],
    ["ANAMNESIS_BINARY", legacy],
    ["ANAMNESIS_MCP_SKIP_DOWNLOAD", "1"],
  ]) {
    const install = runNode(installer, [], { [name]: value, NODE_OPTIONS: `--require=${blockDownload}` });
    assert.strictEqual(install.status, 0, `${name} must skip postinstall download: ${install.stderr}`);
  }

  const canonicalLaunch = runNode(launcher, ["stats", "--json"], {
    ANAMNESIS_MCP_BINARY: canonical,
    ANAMNESIS_BINARY: legacy,
  });
  assert.strictEqual(canonicalLaunch.status, 0, canonicalLaunch.stderr);
  assert.strictEqual(canonicalLaunch.stdout, "canonical:stats --json\n");

  const legacyLaunch = runNode(launcher, ["stats"], {
    ANAMNESIS_MCP_BINARY: "",
    ANAMNESIS_BINARY: legacy,
  });
  assert.strictEqual(legacyLaunch.status, 0, legacyLaunch.stderr);
  assert.strictEqual(legacyLaunch.stdout, "legacy:stats\n");
  const missingCanonical = path.join(tempDir, "missing-canonical");
  const missingCanonicalLaunch = runNode(launcher, [], {
    ANAMNESIS_MCP_BINARY: missingCanonical,
    ANAMNESIS_BINARY: legacy,
  });
  assert.strictEqual(missingCanonicalLaunch.status, 1, missingCanonicalLaunch.stderr);
  assert.ok(
    missingCanonicalLaunch.stderr.includes(`Expected the GitHub Release binary at: ${missingCanonical}`),
    missingCanonicalLaunch.stderr
  );
  assert.match(missingCanonicalLaunch.stderr, /ANAMNESIS_MCP_BINARY/);
} finally {
  fs.rmSync(tempDir, { recursive: true, force: true });
}
