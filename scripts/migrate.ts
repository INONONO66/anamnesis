import { Store } from "../packages/core/src/index.ts";

function required(name: "NEO4J_URI" | "NEO4J_USER" | "NEO4J_PASSWORD"): string {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is required to run schema migrations`);
  return value;
}

const store = new Store({
  uri: required("NEO4J_URI"),
  user: required("NEO4J_USER"),
  password: required("NEO4J_PASSWORD"),
  database: "neo4j",
});

try {
  await store.init();
  console.log("Neo4j schema is ready (constraints and indexes created or already present).");
} finally {
  await store.close();
}
