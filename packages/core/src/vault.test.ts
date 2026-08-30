import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Vault } from "./vault.ts";

let dir: string;
let vault: Vault;

const input = {
  time: { value: "2026-08-21T14:03:22+09:00", precision: "second" },
  content: "이노: 다크 모드가 눈이 편하다",
  origin: {
    source: "kakao-export",
    session: "친구방/2026-08-21",
    actor: "이노",
    record: "msg-000123",
  },
} as const;

beforeEach(() => {
  dir = mkdtempSync(join(tmpdir(), "anamnesis-vault-"));
  vault = new Vault(dir);
});

afterEach(() => {
  vault.close();
  rmSync(dir, { recursive: true, force: true });
});

describe("Vault append", () => {
  test("append 후 동일 내용으로 조회된다", () => {
    const { id, created } = vault.append(input);
    expect(created).toBe(true);
    const rec = vault.get(id)!;
    expect(rec.content).toBe(input.content);
    expect(rec.origin).toEqual(input.origin);
    expect(rec.time).toEqual(input.time);
  });

  test("같은 origin 재유입은 멱등 — 기존 id 반환, 중복 없음", () => {
    const first = vault.append(input);
    const second = vault.append({ ...input, content: "다른 내용이어도" });
    expect(second.created).toBe(false);
    expect(second.id).toBe(first.id);
    expect(vault.count()).toBe(1);
  });

  test("payload는 content-addressed로 보존되고 라운드트립된다", () => {
    const payload = new TextEncoder().encode('{"raw":"…"}');
    const { id } = vault.append({ ...input, payload });
    const rec = vault.get(id)!;
    expect(rec.payloadHash).not.toBeNull();
    expect(vault.getPayload(rec.payloadHash!)).toEqual(payload);
  });

  test("잘못된 입력(빈 content, 오프셋 없는 시간)을 거부한다", () => {
    expect(() => vault.append({ ...input, content: "" })).toThrow();
    expect(() =>
      vault.append({
        ...input,
        time: { value: "2026-08-21T14:03:22", precision: "second" },
      }),
    ).toThrow();
  });
});

describe("Vault outbox", () => {
  test("신규 append만 outbox에 쌓이고, 처리 마킹하면 사라진다", () => {
    const { id } = vault.append(input);
    vault.append(input); // 멱등 재유입 — outbox에 추가되면 안 됨
    const pending = vault.pending();
    expect(pending.length).toBe(1);
    expect(pending[0]!.recordId).toBe(id);

    vault.markProcessed([pending[0]!.seq]);
    expect(vault.pending().length).toBe(0);
  });

  test("resetOutbox는 전 레코드를 재소화 대상으로 되돌린다", () => {
    vault.append(input);
    vault.append({
      ...input,
      origin: { ...input.origin, record: "msg-000124" },
    });
    vault.markProcessed(vault.pending().map((e) => e.seq));
    expect(vault.pending().length).toBe(0);

    vault.resetOutbox();
    expect(vault.pending().length).toBe(2);
  });
});

describe("Vault integrity", () => {
  test("정상 상태에서 verify는 빈 배열", () => {
    vault.append({ ...input, payload: new TextEncoder().encode("raw") });
    expect(vault.verify()).toEqual([]);
  });

  test("오브젝트 파일 변조를 탐지한다", () => {
    const payload = new TextEncoder().encode("original");
    const { id } = vault.append({ ...input, payload });
    const hash = vault.get(id)!.payloadHash!;
    writeFileSync(
      join(dir, "objects", "sha256", hash.slice(0, 2), hash.slice(2)),
      "tampered",
    );
    const issues = vault.verify();
    expect(issues).toEqual([{ recordId: id, kind: "object-hash-mismatch" }]);
  });
});
