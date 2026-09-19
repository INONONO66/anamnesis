import assert from "node:assert/strict";
import { mkdtemp, mkdir, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

const api = () => import(process.env.RESTORE_AUTHORITY_MODULE ?? "./restore-authority.ts");

test("preflight refuses an archive without the complete marker", async () => {
  const { preflightRestore } = await api();
  const root = await mkdtemp(join(tmpdir(), "g005-restore-red-"));
  await writeFile(join(root, "manifest.json"), "{}");
  await assert.rejects(preflightRestore(root, { schema_versions: ["anamnesis.storage/1"], neo4j_versions: ["5.26.0"], neo4j_image_digests: ["sha256:" + "a".repeat(64)], episode_digest_version_ceiling: 1 }), /archive_layout|invalid_completion/);
});

test("activation persists intent before each rename and rejects unknown paths", async () => {
  const { createRestoreActivation, RestoreAuthorityError } = await api();
  const root = await mkdtemp(join(tmpdir(), "g005-restore-red-"));
  const live = join(root, "live"), staging = join(root, "staging"), rollback = join(root, "rollback");
  await mkdir(live); await mkdir(staging); await mkdir(rollback);
  const activation = await createRestoreActivation({ liveRoot: live, stagingRoot: staging, rollbackRoot: rollback, statePath: join(root, "restore.state"), operationId: "018f4c8e-6d43-7abc-8abc-111111111111" });
  await writeFile(join(rollback, "unexpected"), "x");
  await assert.rejects(activation.renameLive(), { code: "unsafe_path" });
  await assert.rejects(activation.promoteStaging(), { code: "unsafe_path" });
  assert.ok(RestoreAuthorityError);
});

test("activation follows the fenced rename boundaries", async () => {
  const { createRestoreActivation } = await api();
  const root = await mkdtemp(join(tmpdir(), "g005-restore-flow-"));
  const live = join(root, "live"), staging = join(root, "staging"), rollback = join(root, "rollback");
  await mkdir(live); await mkdir(staging); await mkdir(rollback);
  const a = await createRestoreActivation({ liveRoot: live, stagingRoot: staging, rollbackRoot: rollback, statePath: join(root, "restore.state"), operationId: "018f4c8e-6d43-7abc-8abc-222222222222" });
  assert.equal((await a.renameLive()).phase, "LIVE_RENAMED");
  assert.equal((await a.promoteStaging()).phase, "STAGING_PROMOTED");
  assert.equal((await a.markWillStart()).phase, "WILL_START");
  assert.equal((await a.markStarted({ source: "trusted-adapter", healthy: true })).phase, "STARTED");
});
