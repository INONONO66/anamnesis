// Gate: Halstead difficulty per function stays under 80. Difficulty is (distinct operators / 2) * (operand uses / distinct
// operands) over the tokens of the function's own scope; a nested function is measured on its own, as eslint measures
// complexity. Identifiers, literals, this, true, false and null are operands; every other keyword or punctuation token is
// an operator, closing brackets excluded. Comments are not tokens.
import { readFileSync } from "node:fs";
import ts from "typescript";
import { sourceFiles } from "./sources.ts";
const LIMIT = 80;
const files = sourceFiles();
const operandKinds = new Set([ts.SyntaxKind.Identifier, ts.SyntaxKind.PrivateIdentifier, ts.SyntaxKind.NumericLiteral, ts.SyntaxKind.BigIntLiteral,
  ts.SyntaxKind.StringLiteral, ts.SyntaxKind.RegularExpressionLiteral, ts.SyntaxKind.NoSubstitutionTemplateLiteral, ts.SyntaxKind.TemplateHead,
  ts.SyntaxKind.TemplateMiddle, ts.SyntaxKind.TemplateTail, ts.SyntaxKind.ThisKeyword, ts.SyntaxKind.TrueKeyword, ts.SyntaxKind.FalseKeyword, ts.SyntaxKind.NullKeyword]);
const closing = new Set([ts.SyntaxKind.CloseParenToken, ts.SyntaxKind.CloseBracketToken, ts.SyntaxKind.CloseBraceToken]);
// Spelled out because ts.isFunctionLikeDeclaration is internal to the compiler API (absent from its public typings).
const isFunction = (node: ts.Node): node is ts.FunctionLikeDeclaration => ts.isFunctionDeclaration(node) || ts.isFunctionExpression(node)
  || ts.isArrowFunction(node) || ts.isMethodDeclaration(node) || ts.isConstructorDeclaration(node) || ts.isGetAccessorDeclaration(node) || ts.isSetAccessorDeclaration(node);
interface Vocabulary { operators: Set<string>; operands: Map<string, number> }
function collect(node: ts.Node, source: ts.SourceFile, into: Vocabulary): void {
  for (const child of node.getChildren(source)) {
    if (child.kind >= ts.SyntaxKind.FirstJSDocNode && child.kind <= ts.SyntaxKind.LastJSDocNode) continue;
    if (isFunction(child) && child.body) continue;
    if (child.getChildCount(source) > 0 || child.kind === ts.SyntaxKind.SyntaxList) { collect(child, source, into); continue; }
    if (closing.has(child.kind)) continue;
    const text = child.getText(source);
    if (operandKinds.has(child.kind)) into.operands.set(text, (into.operands.get(text) ?? 0) + 1); else into.operators.add(text);
  }
}
function difficulty(node: ts.Node, source: ts.SourceFile): number {
  const vocabulary: Vocabulary = { operators: new Set(), operands: new Map() };
  collect(node, source, vocabulary);
  const uses = [...vocabulary.operands.values()].reduce((sum, n) => sum + n, 0);
  return vocabulary.operands.size === 0 ? 0 : (vocabulary.operators.size / 2) * (uses / vocabulary.operands.size);
}
const scores: Array<{ where: string; score: number }> = [];
for (const file of files) {
  const source = ts.createSourceFile(file, readFileSync(file, "utf8"), ts.ScriptTarget.Latest, true);
  const visit = (node: ts.Node): void => {
    if (isFunction(node) && node.body) {
      const name = ts.isConstructorDeclaration(node) ? "constructor" : node.name?.getText(source) ?? "(anonymous)";
      scores.push({ where: `${file}:${source.getLineAndCharacterOfPosition(node.getStart(source)).line + 1} ${name}`, score: difficulty(node, source) });
    }
    ts.forEachChild(node, visit);
  };
  visit(source);
}
const hits = scores.filter(entry => entry.score >= LIMIT);
if (hits.length) { console.error(hits.map(entry => `${entry.where} difficulty ${entry.score.toFixed(1)}`).join("\n")); console.error(`gate:halstead failed: ${hits.length} function(s) at or above ${LIMIT}`); process.exit(1); }
const top = scores.sort((a, b) => b.score - a.score).slice(0, 3).map(entry => `${entry.where} ${entry.score.toFixed(1)}`).join("; ");
console.log(`gate:halstead ok: ${scores.length} functions under ${LIMIT} in ${files.length} files (highest: ${top})`);
