export { API, SymbolFlags, TypeFlags, type Checker, type Type, type Project, type Snapshot, type Symbol } from "typescript/unstable/async";
export {
  SyntaxKind, isExportSpecifier, isImportSpecifier, isExportDeclaration, isImportDeclaration,
  isImportClause, isVariableDeclaration, isExportAssignment, isCallExpression, isIdentifier,
  isObjectLiteralExpression, isPropertyAssignment, isArrowFunction, isFunctionExpression,
  isStringLiteral, isParenthesizedExpression, isAsExpression, isSatisfiesExpression,
  isNonNullExpression, isBlock, isReturnStatement, isShorthandPropertyAssignment,
} from "typescript/unstable/ast";

export function isDateLibrary(path: string): boolean {
  return /\/node_modules\/(?:typescript|@typescript\/typescript-[^/]+)\/lib\/lib\.[^/]+\.d\.ts$/i.test(path);
}
