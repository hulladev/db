import { API, SymbolFlags, SyntaxKind, isExportSpecifier, isImportSpecifier, isExportDeclaration, isImportDeclaration, isImportClause, type Project, type Snapshot, type Symbol } from "./development-compiler.ts";
import { dirname } from 'node:path';
import type { Node, SourceFile } from "typescript/unstable/ast";
import { isVariableDeclaration, isExportAssignment, isCallExpression, isIdentifier, isObjectLiteralExpression, isPropertyAssignment, isShorthandPropertyAssignment, isArrowFunction, isFunctionExpression, isStringLiteral, isParenthesizedExpression, isAsExpression, isSatisfiesExpression, isNonNullExpression, isBlock, isReturnStatement } from "./development-compiler.ts";

interface SourceSpan { readonly file: string; readonly start: number; readonly end: number }
export interface DatabaseDeclaration {
  readonly exportName: string;
  readonly call: SourceSpan;
  readonly factory: SourceSpan;
  readonly argument: SourceSpan;
  readonly schemaExport?: { readonly file: string; readonly name: string };
  readonly factoryExport?: { readonly file: string; readonly name: string };
  readonly functions?: SourceSpan & { readonly shorthand?: true } & ({ readonly kind: "glob"; readonly pattern: string } | { readonly kind: "import"; readonly specifier: string; readonly importer: string });
}

export interface FunctionExport {
  readonly name: string;
  readonly symbol: Symbol;
  readonly original: boolean;
}

/** One compiler session shared by discovery, selection and contract inference. */
export class FunctionAnalysis {
  private readonly api = new API();
  private snapshot?: Snapshot;
  private readonly projects = new Map<string, Project>();
  private readonly exports = new Map<string, readonly FunctionExport[]>();
  private readonly symbols = new Map<SourceFile, readonly Symbol[]>();
  private readonly runtime = new Map<SourceFile, Set<string>>();
  private readonly declarationFiles = new Map<Project, readonly string[]>();
  private readonly brands = new Map<Project, Map<number, boolean>>();
  private readonly opened = new Set<string>();
  private readonly databases = new Map<string, readonly DatabaseDeclaration[]>();

  async open(files: readonly string[]): Promise<void> {
    if (this.snapshot && files.every(file => this.opened.has(file))) return;
    for (const file of files) this.opened.add(file);
    const previous = this.snapshot;
    this.snapshot = await this.api.updateSnapshot({ openFiles: [...this.opened] });
    this.projects.clear(); this.exports.clear(); this.symbols.clear(); this.runtime.clear(); this.declarationFiles.clear(); this.brands.clear();
    this.databases.clear();
    await previous?.dispose();
  }

  async project(source: string): Promise<Project> {
    let project = this.projects.get(source);
    if (!project) {
      project = await this.snapshot?.getDefaultProjectForFile(source);
      if (!project) throw new TypeError(`No TypeScript project for ${source}`);
      this.projects.set(source, project);
    }
    return project;
  }

  private async moduleExports(project: Project, file: SourceFile): Promise<readonly Symbol[]> {
    let symbols = this.symbols.get(file);
    if (!symbols) {
      const module = await project.checker.getSymbolAtLocation(file);
      symbols = module ? await project.checker.getExportsOfModule(module) : [];
      // Native TS can expose no SourceFile module symbol for an object assigned
      // to module.exports. Its checked RHS still provides the named exports.
      for (const statement of file.statements) {
        if (statement.kind !== SyntaxKind.ExpressionStatement || !('expression' in statement)) continue;
        const expression = statement.expression as Node;
        if (expression.kind !== SyntaxKind.BinaryExpression || !('left' in expression) || !('right' in expression)) continue;
        const left = expression.left as Node;
        if (left.kind !== SyntaxKind.PropertyAccessExpression || !('expression' in left) || !('name' in left)) continue;
        const target = left.expression as Node;
        const name = left.name;
        if (!isIdentifier(target) || target.text !== 'module' || !name || typeof name !== 'object' || !('text' in name) || name.text !== 'exports') continue;
        const type = await project.checker.getTypeAtLocation(expression.right as Node);
        if (type) {
          symbols = await project.checker.getPropertiesOfType(type);
          this.runtime.set(file, new Set(symbols.map(value => value.name)));
        }
      }
      const common = symbols.find(symbol => symbol.name === 'export=');
      if (common) {
        const type = await project.checker.getTypeOfSymbol(common);
        if (type) symbols = [...symbols, ...await project.checker.getPropertiesOfType(type)];
      }
      this.symbols.set(file, symbols);
    }
    return symbols;
  }

  private async moduleFile(project: Project, specifier: Node): Promise<SourceFile | undefined> {
    const symbol = await project.checker.getSymbolAtLocation(specifier);
    const declaration = symbol?.declarations[0];
    return declaration ? project.program.getSourceFile(declaration.path) : undefined;
  }
  private requiredBinding(node: Node): { specifier: Node & { text: string }; importer: string; name: string } | undefined {
    let initializer: Node | undefined, name: string | undefined;
    if (node.kind === SyntaxKind.BindingElement && node.parent?.parent && isVariableDeclaration(node.parent.parent)) {
      initializer = node.parent.parent.initializer;
      const binding = 'propertyName' in node && node.propertyName ? node.propertyName : 'name' in node ? node.name : undefined;
      if (binding && typeof binding === 'object' && 'text' in binding && typeof binding.text === 'string') name = binding.text;
    } else if (isVariableDeclaration(node) && node.initializer?.kind === SyntaxKind.PropertyAccessExpression && 'expression' in node.initializer && 'name' in node.initializer) {
      initializer = node.initializer.expression as Node;
      const binding = node.initializer.name;
      if (binding && typeof binding === 'object' && 'text' in binding && typeof binding.text === 'string') name = binding.text;
    }
    if (!initializer || !name || !isCallExpression(initializer) || !isIdentifier(initializer.expression) || initializer.expression.text !== 'require' || !initializer.arguments[0] || !isStringLiteral(initializer.arguments[0])) return undefined;
    let root = node;
    while (root.parent) root = root.parent;
    return { specifier: initializer.arguments[0], importer: (root as SourceFile).fileName, name };
  }
  private async valueBinding(project: Project, node: Node, seen: Set<string>): Promise<boolean> {
    let originalName: string | undefined;
    for (let parent: Node | undefined = node; parent; parent = parent.parent) {
      if ("isTypeOnly" in parent && parent.isTypeOnly) return false;
      if (isImportClause(parent) && parent.phaseModifier === SyntaxKind.TypeKeyword) return false;
      if (isExportSpecifier(parent) || isImportSpecifier(parent)) {
        originalName = (parent.propertyName ?? parent.name).text;
      }
      if ((isExportDeclaration(parent) || isImportDeclaration(parent)) && parent.moduleSpecifier && originalName) {
        const file = await this.moduleFile(project, parent.moduleSpecifier);
        return !!file && this.runtimeExport(project, file, originalName, seen);
      }
    }
    // `import type { value }; export { value }` must not become an endpoint.
    if (isExportSpecifier(node)) {
      const local = await project.checker.getExportSpecifierLocalTargetSymbol(node);
      if (local && local.flags & SymbolFlags.Alias) {
        for (const declaration of local.declarations) {
          const binding = await declaration.resolve(project);
          if (binding && await this.valueBinding(project, binding, seen)) return true;
        }
        return false;
      }
    }
    return true;
  }

  private async runtimeExport(project: Project, file: SourceFile, name: string, seen = new Set<string>()): Promise<boolean> {
    if (this.runtime.get(file)?.has(name)) return true;
    const key = `${file.path}\0${name}`;
    if (seen.has(key)) return false;
    const nested = new Set(seen).add(key);
    const exported = (await this.moduleExports(project, file)).find(symbol => symbol.name === name);
    if (!exported) return false;
    let found = false;
    for (const declaration of exported.declarations) {
      if (declaration.path !== file.path) continue;
      const node = await declaration.resolve(project);
      if (node && await this.valueBinding(project, node, nested)) { found = true; break; }
    }
    // The checker returns the original symbols for star exports, including
    // `export type *`. Follow only runtime star edges, including through barrels.
    if (!found) for (const statement of file.statements) {
      if (!isExportDeclaration(statement) || statement.isTypeOnly || statement.exportClause || !statement.moduleSpecifier) continue;
      const target = await this.moduleFile(project, statement.moduleSpecifier);
      if (target && await this.runtimeExport(project, target, name, nested)) { found = true; break; }
    }
    if (found) {
      let names = this.runtime.get(file);
      if (!names) this.runtime.set(file, names = new Set());
      names.add(name);
    }
    return found;
  }

  async functions(source: string): Promise<readonly FunctionExport[]> {
    const cached = this.exports.get(source);
    if (cached) return cached;
    const project = await this.project(source);
    const file = await project.program.getSourceFile(source);
    if (!file) throw new TypeError(`Cannot read ${source}`);
    const syntax = await project.program.getSyntacticDiagnostics(source);
    if (syntax.length) throw new TypeError(`Invalid function source ${source}: ${syntax[0]!.text} (TS${syntax[0]!.code})`);
    if (!this.declarationFiles.has(project)) {
      const paths = import.meta.url.endsWith('.ts') ? ['./declarations.ts', '../dist/declarations.d.ts'] : ['./declarations.d.ts', '../src/declarations.ts'];
      const declarations = await Promise.all(paths.map(path => project.program.getSourceFile(Bun.fileURLToPath(new URL(path, import.meta.url)))));
      this.declarationFiles.set(project, declarations.flatMap(value => value ? [value.path] : []));
    }
    const declarationPaths = this.declarationFiles.get(project)!;
    let brands = this.brands.get(project);
    if (!brands) this.brands.set(project, brands = new Map());
    const functions: FunctionExport[] = [];
    if (declarationPaths.length) for (const exported of await this.moduleExports(project, file)) {
      const target = exported.flags & SymbolFlags.Alias ? await project.checker.getAliasedSymbol(exported) : exported;
      if (!(target.flags & SymbolFlags.Value)) continue;
      const type = await project.checker.getTypeOfSymbol(target);
      if (!type) continue;
      let branded = brands.get(type.id);
      if (branded === undefined) {
        branded = (await project.checker.getPropertiesOfType(type)).some(property =>
          property.name.startsWith("__@definitionBrand@") && property.declarations.some(declaration => declarationPaths.includes(declaration.path)));
        brands.set(type.id, branded);
      }
      if (branded && await this.runtimeExport(project, file, exported.name)) functions.push({
        name: exported.name, symbol: exported,
        original: target.name === exported.name && target.declarations.some(declaration => declaration.path === file.path),
      });
    }
    this.exports.set(source, functions);
    return functions;
  }

  /** Locate configuration through exported aliases without evaluating a loader. */
  async databaseDeclarations(source: string): Promise<readonly DatabaseDeclaration[]> {
    const cached = this.databases.get(source);
    if (cached) return cached;
    const project = await this.project(source);
    const file = await project.program.getSourceFile(source);
    if (!file) throw new TypeError(`Cannot read ${source}`);
    const results = new Map<string, DatabaseDeclaration>();
    const span = (node: Node): SourceSpan => {
      let root = node;
      while (root.parent) root = root.parent;
      return { file: (root as SourceFile).fileName, start: node.pos, end: node.end };
    };
    const follow = async (node: Node, seen = new Set<string>()): Promise<Node> => {
      if (isParenthesizedExpression(node) || isAsExpression(node) || isSatisfiesExpression(node) || isNonNullExpression(node)) return follow(node.expression, seen);
      if (isVariableDeclaration(node) && node.initializer) return follow(node.initializer, seen);
      if (isPropertyAssignment(node)) return follow(node.initializer, seen);
      if (isShorthandPropertyAssignment(node)) {
        const symbol = await project.checker.getShorthandAssignmentValueSymbol(node);
        const declaration = symbol?.declarations[0];
        const value = await declaration?.resolve(project);
        if (value) return follow(value, seen);
      }
      if (isExportAssignment(node)) return follow(node.expression, seen);
      if (node.kind === SyntaxKind.BinaryExpression && 'right' in node) return follow(node.right as Node, seen);
      if (node.kind === SyntaxKind.PropertyAccessExpression && node.parent?.kind === SyntaxKind.BinaryExpression && 'right' in node.parent) return follow(node.parent.right as Node, seen);
      if (isIdentifier(node)) {
        let symbol = await project.checker.getSymbolAtLocation(node);
        if (symbol && symbol.flags & SymbolFlags.Alias) symbol = await project.checker.getAliasedSymbol(symbol);
        for (const declaration of symbol?.declarations ?? []) {
          const key = `${declaration.path}:${declaration.index}`;
          if (seen.has(key)) continue;
          const target = await declaration.resolve(project);
          if (target && (isVariableDeclaration(target) || isExportAssignment(target))) return follow(target, new Set(seen).add(key));
        }
      }
      return node;
    };
    const exportedBinding = async (node: Node): Promise<{ file: string; name: string } | undefined> => {
      let symbol = isShorthandPropertyAssignment(node)
        ? await project.checker.getShorthandAssignmentValueSymbol(node)
        : await project.checker.getSymbolAtLocation(node);
      for (const declaration of symbol?.declarations ?? []) {
        const value = await declaration.resolve(project);
        const required = value && this.requiredBinding(value);
        if (required) return { file: Bun.resolveSync(required.specifier.text, dirname(required.importer)), name: required.name };
      }
      // Resolve through the actual import/export conditions, rather than assume
      // a package's .d.ts has a same-directory JavaScript implementation.
      if (symbol && symbol.flags & SymbolFlags.Alias) for (const declaration of symbol.declarations) {
        let imported: string | undefined;
        for (let binding = await declaration.resolve(project); binding; binding = binding.parent) {
          if ('isTypeOnly' in binding && binding.isTypeOnly) return undefined;
          if (isImportSpecifier(binding)) imported = (binding.propertyName ?? binding.name).text;
          else if (isImportClause(binding) && binding.name && imported === undefined) imported = 'default';
          if (isImportDeclaration(binding) && binding.moduleSpecifier && isStringLiteral(binding.moduleSpecifier) && imported) {
            return { file: Bun.resolveSync(binding.moduleSpecifier.text, dirname(span(binding).file)), name: imported };
          }
        }
      }
      if (symbol && symbol.flags & SymbolFlags.Alias) symbol = await project.checker.getAliasedSymbol(symbol);
      for (const declaration of symbol?.declarations ?? []) {
        let root = await declaration.resolve(project);
        if (!root) continue;
        while (root.parent) root = root.parent;
        const file = root as SourceFile;
        for (const exported of await this.moduleExports(project, file)) {
          const target = exported.flags & SymbolFlags.Alias ? await project.checker.getAliasedSymbol(exported) : exported;
          if (target.declarations.some(value => value.path === declaration.path && value.index === declaration.index)) return { file: file.fileName, name: exported.name };
        }
      }
      return undefined;
    };
    const candidates: { name: string; root: Node }[] = [];
    for (const exported of await this.moduleExports(project, file)) {
      const target = exported.flags & SymbolFlags.Alias ? await project.checker.getAliasedSymbol(exported) : exported;
      const type = await project.checker.getTypeOfSymbol(target);
      if (!type || !(await project.checker.getPropertiesOfType(type)).some(property => property.name.startsWith('__@instanceType@'))) continue;
      for (const declaration of target.declarations) {
        const root = await declaration.resolve(project);
        if (root) candidates.push({ name: exported.name, root });
      }
    }
    for (const candidate of candidates) {
        const root = candidate.root;
        const call = await follow(root);
        if (!isCallExpression(call) || !call.arguments[0]) continue;
        const options = await follow(call.arguments[0]);
        if (!isObjectLiteralExpression(options)) continue;
        // AST nodes expose names as text; no source printer or string parser is needed.
        const named = (name: string) => options.properties.find(property => (isPropertyAssignment(property) || isShorthandPropertyAssignment(property)) && "text" in property.name && property.name.text === name);
        const schemaProperty = named("schema");
        if (!schemaProperty || (!isPropertyAssignment(schemaProperty) && !isShorthandPropertyAssignment(schemaProperty))) continue;
        const schemaNode = isPropertyAssignment(schemaProperty) ? schemaProperty.initializer : schemaProperty;
        const functions = named("functions");
        let selection: DatabaseDeclaration["functions"];
        if (functions && (isPropertyAssignment(functions) || isShorthandPropertyAssignment(functions))) {
          const node = isPropertyAssignment(functions) ? functions.initializer : functions;
          const value = await follow(node);
          const location = { ...span(node), ...(isShorthandPropertyAssignment(functions) ? { shorthand: true as const } : {}) };
          if (isIdentifier(value) && value.text === "undefined") { /* Explicit omission. */ }
          else if (isStringLiteral(value)) selection = { ...location, kind: "glob", pattern: value.text };
          else if (isArrowFunction(value) || isFunctionExpression(value)) {
            let body: Node = value.body;
            if (isBlock(body)) {
              if (body.statements.length !== 1 || !isReturnStatement(body.statements[0]!) || !body.statements[0]!.expression) throw new TypeError("functions loader must return one literal import()");
              body = body.statements[0]!.expression;
            }
            body = await follow(body);
            if (body.kind === SyntaxKind.AwaitExpression && "expression" in body) body = body.expression as Node;
            if (!isCallExpression(body) || body.expression.kind !== SyntaxKind.ImportKeyword || body.arguments.length !== 1 || !isStringLiteral(body.arguments[0]!)) throw new TypeError("functions loader must return one literal import()");
            selection = { ...location, kind: "import", specifier: body.arguments[0]!.text, importer: span(body).file };
          } else throw new TypeError("functions must be a glob or a lazy literal import()");
        }
        const location = span(call);
        const schemaBinding = await exportedBinding(schemaNode);
        const schemaExport = schemaBinding?.file !== location.file ? schemaBinding : undefined;
        const factoryNode = call.expression.kind === SyntaxKind.PropertyAccessExpression && "expression" in call.expression ? call.expression.expression as Node : call.expression;
        const factoryExport = await exportedBinding(factoryNode);
        results.set(`${location.file}:${location.start}`, { exportName: candidate.name, call: location, factory: span(call.expression), argument: span(call.arguments[0]), ...(schemaExport ? { schemaExport } : {}), ...(factoryExport ? { factoryExport } : {}), ...(selection ? { functions: selection } : {}) });
    }
    const declarations = [...results.values()];
    this.databases.set(source, declarations);
    return declarations;
  }

  /** Namespace barrel exports describe module groups; flat exports form one module. */
  async catalogue(source: string, prefix: string): Promise<Readonly<Record<string, string>>> {
    const project = await this.project(source);
    const modules: Record<string, string> = Object.create(null);
    const visit = async (path: string, name: string, ancestors: ReadonlySet<string>, root = false) => {
      if (ancestors.has(path)) throw new TypeError(`Cyclic function catalogue namespace: ${name}`);
      const file = await project.program.getSourceFile(path);
      if (!file) throw new TypeError(`Cannot resolve function catalogue ${path}`);
      if ((await this.functions(path)).length) modules[name] = file.fileName;
      for (const exported of await this.moduleExports(project, file)) {
        const target = exported.flags & SymbolFlags.Alias ? await project.checker.getAliasedSymbol(exported) : exported;
        if (!(target.flags & (SymbolFlags.ValueModule | SymbolFlags.NamespaceModule)) || !await this.runtimeExport(project, file, exported.name)) continue;
        const declaration = target.declarations.find(value => value.kind === SyntaxKind.SourceFile);
        if (declaration) await visit(declaration.path, root ? exported.name : `${name}/${exported.name}`, new Set(ancestors).add(path));
      }
    };
    await visit(source, prefix, new Set(), true);
    return modules;
  }

  async close(): Promise<void> {
    try { await this.snapshot?.dispose(); }
    finally { await this.api.close(); }
  }
}
