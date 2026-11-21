import { createReadStream } from "node:fs";

/** LF only: readline also splits on U+2028 / U+2029, which JSON strings may hold unescaped. */
export async function* jsonLines(path: string): AsyncGenerator<string> {
  const decoder = new TextDecoder("utf-8", { fatal: true });
  let carry = "";
  for await (const chunk of createReadStream(path)) {
    const parts = (carry + decoder.decode(chunk as Buffer, { stream: true })).split("\n");
    carry = parts.pop() ?? "";
    yield* parts;
  }
  carry += decoder.decode();
  if (carry !== "") yield carry;
}
