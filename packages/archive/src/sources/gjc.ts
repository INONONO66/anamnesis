import { join } from "node:path";
import { sessionAdapter } from "./omo.ts";

export const gjcAdapter = sessionAdapter("gjc", (root) => join(root, "home", ".gjc", "agent", "sessions"));
