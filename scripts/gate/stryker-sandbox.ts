// Runs inside a Stryker sandbox at the head of its test command. Stryker symlinks every node_modules to
// the project's own, whose @anamnesis/* links resolve back to the unmutated packages, so cross-package
// imports would bypass the mutants. Relink @anamnesis/* into the sandbox; a real directory is left alone.
import { lstat, mkdir, readdir, readlink, rm, symlink } from "node:fs/promises";
import { join, relative, resolve } from "node:path";

const packagesRoot = resolve("packages");

async function relink(dir: string): Promise<void> {
  const modules = join(dir, "node_modules");
  const stat = await lstat(modules).catch(() => null);
  if (!stat?.isSymbolicLink()) return;
  const original = resolve(dir, await readlink(modules));
  const entries = await readdir(original);
  if (!entries.includes("@anamnesis")) return;
  await rm(modules);
  await mkdir(modules);
  for (const entry of entries) if (entry !== "@anamnesis") await symlink(join(original, entry), join(modules, entry));
  // node_modules/@anamnesis/<name> is the workspace link to packages/<name>; the sandbox copy of that package replaces it.
  const scope = join(modules, "@anamnesis");
  await mkdir(scope);
  for (const name of await readdir(join(original, "@anamnesis"))) await symlink(relative(scope, join(packagesRoot, name)), join(scope, name));
}

await relink(".");
for (const name of await readdir("packages")) await relink(join("packages", name));
