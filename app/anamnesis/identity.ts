import { IdentityDirectoryEntry, RpcPeopleShowParams } from "@anamnesis/protocol";
import type { RpcClient } from "./client.ts";
import { jsonLines } from "./json-lines.ts";

const USAGE = "usage: identity status | identity rebuild [--accounts] | identity correct SOURCE:ACTOR SOURCE:ACTOR same|different [--reason TEXT]"
  + " | identity directory <users.jsonl> | people list [--query TEXT] | people show <person-id|SOURCE:ACTOR>";
const print = (value: unknown) => console.log(JSON.stringify(value));

function account(value: string | undefined): { source: string; actor: string } {
  const colon = value?.indexOf(":") ?? -1;
  if (value === undefined || colon <= 0 || colon === value.length - 1) throw new Error(USAGE);
  return { source: value.slice(0, colon), actor: value.slice(colon + 1) };
}

async function importDirectory(client: RpcClient, path: string): Promise<void> {
  let entries: IdentityDirectoryEntry[] = [], imported = 0, changed = 0;
  const flush = async () => {
    if (entries.length === 0) return;
    const result = await client.request("identity.directory", { entries });
    imported += result.imported; changed += result.changed; entries = [];
  };
  for await (const line of jsonLines(path)) {
    if (!line.trim()) continue;
    entries.push(IdentityDirectoryEntry.parse(JSON.parse(line)));
    if (entries.length === 100) await flush();
  }
  await flush();
  print({ imported, changed });
}

async function listPeople(client: RpcClient, query: string | undefined): Promise<void> {
  let after: string | undefined;
  for (;;) {
    const page = await client.request("people.list", { limit: 200, ...(after ? { after } : {}), ...(query ? { query } : {}) });
    for (const person of page.people) print(person);
    if (page.next === null) return;
    after = page.next;
  }
}

async function identity(client: RpcClient, action: string | undefined, rest: readonly string[]): Promise<void> {
  if (action === "status" && rest.length === 0) return print(await client.request("identity.status", {}));
  if (action === "rebuild" && (rest.length === 0 || (rest.length === 1 && rest[0] === "--accounts"))) {
    return print(await client.request("identity.rebuild", { accounts: rest.length === 1 }));
  }
  if (action === "directory" && rest.length === 1) return importDirectory(client, rest[0]!);
  const verdict = rest[2], reason = rest.length === 5 && rest[3] === "--reason" ? rest[4] : undefined;
  if (action !== "correct" || (verdict !== "same" && verdict !== "different") || (rest.length !== 3 && reason === undefined)) throw new Error(USAGE);
  return print(await client.request("identity.correct", { a: account(rest[0]), b: account(rest[1]), verdict, ...(reason ? { reason } : {}) }));
}

async function people(client: RpcClient, action: string | undefined, rest: readonly string[]): Promise<void> {
  if (action === "list" && (rest.length === 0 || (rest.length === 2 && rest[0] === "--query"))) return listPeople(client, rest[1]);
  if (action !== "show" || rest.length !== 1) throw new Error(USAGE);
  const byId = RpcPeopleShowParams.safeParse({ person_id: rest[0] });
  return print(await client.request("people.show", byId.success ? byId.data : { account: account(rest[0]) }));
}

export function identityCommand(client: RpcClient, command: "identity" | "people", args: readonly string[]): Promise<void> {
  const [action, ...rest] = args;
  return command === "identity" ? identity(client, action, rest) : people(client, action, rest);
}
