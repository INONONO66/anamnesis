/** Ensures the isolated Neo4j test container is running and reachable. */
import { execSync } from "node:child_process";
import neo4j from "neo4j-driver";

const NAME = "anamnesis-neo4j-test";
const URI = "bolt://127.0.0.1:7688";
const PASSWORD = "anamnesis-test";

function sh(cmd: string): string {
  return execSync(cmd, { encoding: "utf8" }).trim();
}

function containerState(): "running" | "stopped" | "absent" {
  try {
    const s = sh(`docker inspect -f '{{.State.Running}}' ${NAME} 2>/dev/null`);
    return s === "true" ? "running" : "stopped";
  } catch {
    return "absent";
  }
}

const state = containerState();
if (state === "stopped") sh(`docker start ${NAME}`);
if (state === "absent") {
  sh(
    [
      `docker run -d --name ${NAME}`,
      `-p 127.0.0.1:7688:7687`,
      `-e NEO4J_AUTH=neo4j/${PASSWORD}`,
      `-e NEO4J_server_memory_heap_max__size=1G`,
      `-e NEO4J_server_memory_pagecache_size=512M`,
      `neo4j:5.26-community`,
    ].join(" "),
  );
}

// A cold image pull and JVM startup can take several minutes.
const driver = neo4j.driver(URI, neo4j.auth.basic("neo4j", PASSWORD));
const deadline = Date.now() + 180_000;
for (;;) {
  try {
    await driver.verifyConnectivity();
    break;
  } catch (e) {
    if (Date.now() > deadline) {
      console.error(`Neo4j test container not ready in 180s: ${e}`);
      process.exit(1);
    }
    await new Promise((r) => setTimeout(r, 1000));
  }
}
await driver.close();
console.log(`neo4j test db ready at ${URI}`);
