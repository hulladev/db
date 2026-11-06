import type * as TypeScript from "typescript/unstable/async";

const source = import.meta.url.endsWith("/src/development-compiler.ts");
const compiler: typeof TypeScript = await import(source ? "typescript/unstable/async"
  : new URL("../development/compiler/dist/api/async/api.js", import.meta.url).href);

/** The compiler and its standard library belong to our tools, not the application. */
export class API extends compiler.API {
  constructor() {
    super(source ? {} : { tsserverPath: Bun.fileURLToPath(new URL(`../native/${process.platform}-${process.arch}/compiler/lib/tsc`, import.meta.url)) });
  }
}
export const { SymbolFlags, TypeFlags } = compiler;
const ast: typeof import("typescript/unstable/ast") = await import(source ? "typescript/unstable/ast"
  : new URL("../development/compiler/dist/ast/index.js", import.meta.url).href);
export const { SyntaxKind, isExportSpecifier, isImportSpecifier, isExportDeclaration, isImportDeclaration, isImportClause } = ast;
export const { isVariableDeclaration, isExportAssignment, isCallExpression, isIdentifier, isObjectLiteralExpression, isPropertyAssignment, isArrowFunction, isFunctionExpression, isStringLiteral, isParenthesizedExpression, isAsExpression, isSatisfiesExpression, isNonNullExpression, isBlock, isReturnStatement } = ast;
export const { isShorthandPropertyAssignment } = ast;
export type { Checker, Type, Project, Snapshot, Symbol } from "typescript/unstable/async";

export function isDateLibrary(path: string): boolean {
  return /\/(?:node_modules\/(?:typescript|@typescript\/typescript-[^/]+)|native\/[^/]+\/compiler)\/lib\/lib\.[^/]+\.d\.ts$/i.test(path);
}
