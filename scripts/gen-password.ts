import { randomBytes } from "node:crypto";
import { open, readFile, stat } from "node:fs/promises";
import { join } from "node:path";

function isMissing(error: Error): boolean {
  return "code" in error && error.code === "ENOENT";
}

async function exists(path: string): Promise<boolean> {
  try {
    await stat(path);
    return true;
  } catch (error) {
    if (error instanceof Error && isMissing(error)) return false;
    throw error;
  }
}

const envPath = join(process.cwd(), ".env");
if (await exists(envPath)) {
  console.log(".env already exists; not overwriting it.");
} else {
  const template = await readFile(join(process.cwd(), ".env.example"), "utf8");
  const password = randomBytes(32).toString("base64url");
  const content = template.replace(/^NEO4J_PASSWORD=.*$/m, `NEO4J_PASSWORD=${password}`);
  if (content === template) {
    throw new Error(".env.example must define NEO4J_PASSWORD");
  }

  let file;
  try {
    file = await open(envPath, "wx", 0o600);
  } catch (error) {
    if (error instanceof Error && "code" in error && error.code === "EEXIST") {
      console.log(".env already exists; not overwriting it.");
      process.exit(0);
    }
    throw error;
  }
  try {
    await file.writeFile(content);
    await file.sync();
    await file.chmod(0o600);
  } finally {
    await file.close();
  }
  console.log("Generated .env with a random Neo4j password.");
}
