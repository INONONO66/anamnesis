import { test, expect } from "bun:test";
import { createServer } from "node:net";
import { once } from "node:events";
import { mkdtemp, rm } from "node:fs/promises";
import { RpcClient } from "../../app/anamnesis/client.ts";
import { Frames, encode } from "../../app/anamnesis/wire.ts";
import { capabilities } from "../../app/anamnesis/runtime.ts";
import { RPC_LIMITS } from "../../packages/protocol/src/rpc.ts";
import { randomUUID } from "node:crypto";

test("lost response rejects as machine-readable UNKNOWN without automatic retry", async () => {
  const root = await mkdtemp('/tmp/ana-unknown-');
  let remembers = 0;
  const server = createServer(socket => {
    const frames = new Frames(bytes => {
      const request = JSON.parse(bytes.toString());
      if (request.method === 'hello') socket.write(encode({jsonrpc:'2.0',id:request.id,method:'hello',structure_revision:null,policy_revision:null,server_time:1,result:{version:1,principal:'installation',commit_mode:'receipt',data_incarnation:randomUUID(),fs_epoch:randomUUID(),capabilities,limits:{frame_bytes:RPC_LIMITS.frame_bytes,chunk_bytes:RPC_LIMITS.chunk_bytes,object_bytes:RPC_LIMITS.object_bytes,content_bytes:RPC_LIMITS.content_bytes}}}));
      else { remembers++; socket.destroy(); }
      return true;
    });
    socket.on('data', (bytes: Buffer) => frames.push(bytes));
  });
  let client: RpcClient | undefined;
  try {
    const listening = once(server, 'listening', {signal:AbortSignal.timeout(3000)});
    server.listen(root+'/socket'); await listening;
    client = await RpcClient.connect(root+'/socket','token');
    const error = await client.request('remember',{episode:{schema:'anamnesis.original-message/1',content:'lost',time:{value:'2026-09-09T00:00:00Z',precision:'second'},origin:{source:'test',session:'s',actor:'a',record:'r'},mass:1},source_revision:'v1',expected_previous_revision_key:null}).then(()=>null, error=>error);
    expect(error?.code).toBe('outcome_unknown');
    expect(error?.retryable).toBe(false);
    expect(remembers).toBe(1);
  } finally {
    await client?.close();
    await new Promise<void>((resolve,reject)=>server.close(error=>error?reject(error):resolve()));
    await rm(root,{recursive:true,force:true});
  }
}, 10000);
