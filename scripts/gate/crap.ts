// Gate: CRAP (change risk anti-patterns) per function stays under 25. CRAP = CC^2 * (1 - coverage)^3 + CC, where CC is the
// function's cyclomatic complexity (the decision points eslint's complexity rule counts) and coverage is the fraction of the
// function's lcov-reported lines any given lcov file executed. Pass one or more lcov.info paths; records for one source
// union across files, so a pure-suite report and an owned-Neo4j report together measure the real coverage. A function whose
// source no lcov file mentions counts as uncovered. Scopes are measured as eslint measures them: every function, class
// static block and class field initializer on its own, with nested functions excluded from the parent's decision count. A
// parent's coverage span excludes the interior lines of its nested scopes (their boundary lines stay with the parent), so a
// well-covered callback cannot raise an otherwise unexecuted parent; a span without any reported line counts as uncovered.
import { readFileSync } from "node:fs";
import { relative } from "node:path";
import ts from "typescript";
import { sourceFiles } from "./sources.ts";
const LIMIT = 25;
const lcovPaths = process.argv.slice(2);
if (lcovPaths.length === 0) { console.error("usage: bun scripts/gate/crap.ts <lcov.info> [more lcov.info]"); process.exit(2); }
const files = sourceFiles();
const shipped = new Set(files);
interface Lines { hit: Set<number>; reported: Set<number> }
const coverage = new Map<string, Lines>();
for (const path of lcovPaths) {
  let current: Lines | undefined;
  for (const raw of readFileSync(path, "utf8").split("\n")) {
    const line = raw.trim();
    if (line.startsWith("SF:")) {
      const file = relative(process.cwd(), line.slice(3));
      current = shipped.has(file) ? coverage.get(file) ?? { hit: new Set(), reported: new Set() } : undefined;
      if (current) coverage.set(file, current);
    } else if (current && line.startsWith("DA:")) {
      // DA:<line>,<count>[,<checksum>]
      const fields = line.slice(3).split(",");
      const number = Number(fields[0]), count = Number(fields[1]);
      if (!Number.isFinite(number) || !Number.isFinite(count)) continue; // malformed record: neither reported nor hit
      current.reported.add(number);
      if (count > 0) current.hit.add(number);
    }
  }
}
if (coverage.size === 0) { console.error("gate:crap failed: no lcov record matched a shipped source file (SF paths must resolve relative to the repo root)"); process.exit(2); }
const empty = [...coverage].filter(([, lines]) => lines.reported.size === 0).map(([file]) => file);
if (empty.length) { console.error(`gate:crap failed: lcov lists ${empty.length} shipped file(s) without any DA record: ${empty.join(", ")}`); process.exit(2); }
// Spelled out because ts.isFunctionLikeDeclaration is internal to the compiler API (absent from its public typings).
const isFunction = (node: ts.Node): node is ts.FunctionLikeDeclaration => ts.isFunctionDeclaration(node) || ts.isFunctionExpression(node)
  || ts.isArrowFunction(node) || ts.isMethodDeclaration(node) || ts.isConstructorDeclaration(node) || ts.isGetAccessorDeclaration(node) || ts.isSetAccessorDeclaration(node);
/** A scope eslint scores on its own: a function with a body, a class static block, or a class field initializer. */
const isScope = (node: ts.Node): boolean => (isFunction(node) && node.body !== undefined) || ts.isClassStaticBlockDeclaration(node)
  || (ts.isPropertyDeclaration(node) && node.initializer !== undefined);
const branching = new Set([ts.SyntaxKind.IfStatement, ts.SyntaxKind.ConditionalExpression, ts.SyntaxKind.ForStatement, ts.SyntaxKind.ForInStatement,
  ts.SyntaxKind.ForOfStatement, ts.SyntaxKind.WhileStatement, ts.SyntaxKind.DoStatement, ts.SyntaxKind.CaseClause, ts.SyntaxKind.CatchClause]);
const shortCircuit = new Set([ts.SyntaxKind.AmpersandAmpersandToken, ts.SyntaxKind.BarBarToken, ts.SyntaxKind.QuestionQuestionToken,
  ts.SyntaxKind.AmpersandAmpersandEqualsToken, ts.SyntaxKind.BarBarEqualsToken, ts.SyntaxKind.QuestionQuestionEqualsToken]);
interface Span { from: number; to: number }
const span = (node: ts.Node, source: ts.SourceFile): Span => ({
  from: source.getLineAndCharacterOfPosition(node.getStart(source)).line + 1, to: source.getLineAndCharacterOfPosition(node.end).line + 1,
});
/** Decision points of the scope itself plus the spans of its direct nested scopes, which are measured separately. */
function decisions(node: ts.Node, source: ts.SourceFile): { count: number; nested: Span[] } {
  let count = 0; const nested: Span[] = [];
  const visit = (child: ts.Node): void => {
    if (isScope(child)) { nested.push(span(child, source)); return; }
    if (branching.has(child.kind)) count++;
    else if (ts.isBinaryExpression(child) && shortCircuit.has(child.operatorToken.kind)) count++;
    else if ((ts.isParameter(child) || ts.isBindingElement(child)) && child.initializer) count++;
    else if ((ts.isPropertyAccessExpression(child) || ts.isElementAccessExpression(child) || ts.isCallExpression(child)) && child.questionDotToken) count++;
    ts.forEachChild(child, visit);
  };
  ts.forEachChild(node, visit);
  return { count, nested };
}
/** Fraction of the scope's own reported lines that ran: interior lines of nested scopes are theirs, not the parent's. A
 * nested scope's boundary lines count for both sides on purpose; both read the same hit bit, so neither side is inflated. */
function covered(file: string, own: Span, nested: Span[]): number {
  const lines = coverage.get(file);
  if (!lines) return 0;
  let reported = 0, hit = 0;
  for (let line = own.from; line <= own.to; line++) {
    if (!lines.reported.has(line) || nested.some(inner => inner.from < line && line < inner.to)) continue;
    reported++; if (lines.hit.has(line)) hit++;
  }
  return reported === 0 ? 0 : hit / reported;
}
const scopeName = (node: ts.Node, source: ts.SourceFile): string => ts.isConstructorDeclaration(node) ? "constructor" : ts.isClassStaticBlockDeclaration(node) ? "static"
  : (isFunction(node) || ts.isPropertyDeclaration(node)) ? node.name?.getText(source) ?? "(anonymous)" : "(anonymous)";
const scores: Array<{ where: string; crap: number; cc: number; cov: number }> = [];
for (const file of files) {
  const source = ts.createSourceFile(file, readFileSync(file, "utf8"), ts.ScriptTarget.Latest, true);
  const visit = (node: ts.Node): void => {
    if (isScope(node)) {
      const own = span(node, source), { count, nested } = decisions(node, source);
      const cc = count + 1, cov = covered(file, own, nested), column = source.getLineAndCharacterOfPosition(node.getStart(source)).character + 1;
      scores.push({ where: `${file}:${own.from}:${column} ${scopeName(node, source)}`, crap: cc * cc * (1 - cov) ** 3 + cc, cc, cov });
    }
    ts.forEachChild(node, visit);
  };
  visit(source);
}
const hits = scores.filter(entry => entry.crap >= LIMIT).sort((a, b) => b.crap - a.crap);
const describe = (entry: typeof scores[number]): string => `${entry.where} crap ${entry.crap.toFixed(1)} (cc ${entry.cc}, coverage ${(entry.cov * 100).toFixed(0)}%)`;
if (hits.length) { console.error(hits.map(describe).join("\n")); console.error(`gate:crap failed: ${hits.length} of ${scores.length} scope(s) at or above ${LIMIT}`); process.exit(1); }
const top = scores.sort((a, b) => b.crap - a.crap).slice(0, 3).map(describe).join("; ");
console.log(`gate:crap ok: ${scores.length} scopes under ${LIMIT} in ${files.length} files (highest: ${top})`);
