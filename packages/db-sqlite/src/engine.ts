import { DatabaseError } from "@hulla/db/internal/engine";
import { compileSchema, type CompiledTable, type SchemaDefinition } from "@hulla/db/internal/engine";
import { schemaDescriptor } from "@hulla/db/internal/engine";
import type { Predicate } from "@hulla/db/internal/engine";
import type { CompiledConflict, WriteExpression } from "@hulla/db/internal/engine";
import type { AggregateDescriptor } from "@hulla/db/internal/engine";
import type { SQLiteDriver, SQLiteRecord, SQLiteValue } from "./driver.ts";

export type StoredRow = Record<string, unknown>;
export const sqliteFailure = (code: string, message: string) => new DatabaseError({ code, message });
const quote = (value: string) => '"' + value.replaceAll('"', '""') + '"';
const literal = (value: SQLiteValue) => value === null ? "NULL" : typeof value === "number" ? String(value) : "'" + value.replaceAll("'", "''") + "'";
const checkLiteral = (value: string | number | bigint | boolean | null) => typeof value === "bigint" ? String(value) : typeof value === "boolean" ? String(Number(value)) : literal(value);
type Sort = { field: string; direction: "asc" | "desc" };

export class SQLiteEngine {
  readonly tables: readonly CompiledTable[];
  readonly contract: string;
  readonly driver: SQLiteDriver;
  readonly randomBytes: (length: number) => Uint8Array;
  readonly #layout: Map<string, { table: CompiledTable; physical: string; columns: Map<string, string> }>;
  source = "";
  constructor(driver: SQLiteDriver, schema: SchemaDefinition, randomBytes: (length: number) => Uint8Array = length => {
    if (!globalThis.crypto?.getRandomValues) throw new TypeError("Supply randomBytes using a cryptographically secure platform RNG (for example expo-crypto)");
    return globalThis.crypto.getRandomValues(new Uint8Array(length));
  }) {
    this.driver = driver;
    this.randomBytes = randomBytes;
    this.tables = compileSchema(schema);
    this.#layout = new Map(this.tables.map((table, index) => [table.name, {
      table, physical: quote(`_hulla_t${index}`),
      columns: new Map(Object.keys(table.fields).map((name, index) => [name, quote(`c${index}`)])),
    }]));
    this.contract = JSON.stringify(schemaDescriptor(schema));
    if (this.tables.some(table => typeof table.keyGeneration === "object"))
      throw new TypeError("SQLite custom key handlers are not supported; supply explicit keys with keyGeneration: 'none'");
  }
  execute(sql: string, parameters: readonly SQLiteValue[] = []) { return Promise.resolve(this.driver.execute(sql, parameters)); }
  table(name: string) {
    const table = this.#layout.get(name)?.table;
    if (!table) throw new TypeError(`Unknown table ${name}`);
    return table;
  }
  physical(table: CompiledTable) { return this.#layout.get(table.name)!.physical; }
  column(table: CompiledTable, name: string) {
    const column = this.#layout.get(table.name)!.columns.get(name);
    if (column === undefined) throw new TypeError(`Unknown field ${table.name}.${name}`);
    return column;
  }
  bind(table: CompiledTable, name: string, value: unknown, parameters: SQLiteValue[]): string {
    const field = table.fields[name]!;
    if (value === null) parameters.push(null);
    else if (field.scalar === "json") parameters.push(JSON.stringify(value));
    else if (field.scalar === "boolean") parameters.push(value ? 1 : 0);
    else if (["string", "number"].includes(typeof value)) parameters.push(value as string | number);
    else throw new TypeError("Invalid SQLite wire value");
    return field.scalar === "int64" ? "CAST(? AS INTEGER)" : "?";
  }
  fields(table: CompiledTable, prefix = "") {
    return [...this.#layout.get(table.name)!.columns].map(([name, physical]) => {
      const column = prefix + physical;
      return `${table.fields[name]!.scalar === "int64" ? `CAST(${column} AS TEXT)` : column} AS ${quote(name)}`;
    }).join(", ");
  }
  decode(table: CompiledTable, value: SQLiteRecord): StoredRow {
    return Object.fromEntries(Object.entries(table.fields).map(([name, field]) => {
      let item: unknown = value[name];
      if (item !== null && field.scalar === "json") item = JSON.parse(item as string);
      if (item !== null && field.scalar === "boolean") item = item === 1;
      // Decode checks storage constraints and codecs, but transport stays in wire representation.
      field.decode(item);
      return [name, item];
    }));
  }
  envelope(row: StoredRow, projection?: string[]) {
    return { row: projection ? Object.fromEntries(projection.map(name => [name, row[name]])) : row, version: "1" };
  }
  private async configure() {
    await this.execute("PRAGMA foreign_keys = ON");
    const flags = await this.execute("PRAGMA foreign_keys");
    if (flags[0]?.foreign_keys !== 1) throw sqliteFailure("sqlite_configuration", "SQLite must enforce foreign keys");
    await this.execute("PRAGMA busy_timeout = 5000");
  }
  async verify() {
    await this.configure();
    const exists = await this.execute("SELECT name FROM sqlite_master WHERE type = 'table' AND name = '_hulla_meta'");
    if (!exists.length) throw sqliteFailure("database_uninitialized", "Initialize SQLite explicitly before connecting");
    const meta = await this.execute("SELECT contract, source FROM _hulla_meta");
    if (meta.length !== 1 || meta[0]?.contract !== this.contract) throw sqliteFailure("schema_mismatch", "SQLite schema differs; migrate explicitly without resetting existing data");
    this.source = String(meta[0]!.source);
  }
  async initialize() {
    await this.configure();
    await this.execute("BEGIN IMMEDIATE");
    try {
      const existing = await this.execute("SELECT name FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'");
      if (existing.length) { await this.verify(); }
      else {
        this.source = uuid(true, this.randomBytes);
        await this.execute("CREATE TABLE _hulla_meta (contract TEXT NOT NULL, source TEXT NOT NULL) STRICT");
        await this.execute("CREATE TABLE _hulla_client (sequence INTEGER PRIMARY KEY AUTOINCREMENT, namespace TEXT NOT NULL, kind TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL, UNIQUE(namespace,kind,key)) STRICT");
        await this.execute("INSERT INTO _hulla_meta VALUES (?, ?)", [this.contract, this.source]);
        for (const table of this.tables) await this.createTable(table);
      }
      await this.execute("COMMIT");
    } catch (error) { await this.execute("ROLLBACK"); throw error; }
  }
  private async createTable(table: CompiledTable) {
    const definitions = Object.entries(table.fields).map(([name, field]) => {
      const column = this.column(table, name);
      const type = field.scalar === "int64" || field.scalar === "boolean" ? "INTEGER" : field.scalar === "float64" ? "REAL" : "TEXT";
      const checks: string[] = [];
      if (field.scalar === "boolean") checks.push(`${column} IN (0, 1)`);
      if (field.scalar === "json") checks.push(`json_valid(${column})`);
      if (field.constraints?.min !== undefined) checks.push(`${field.scalar === "string" ? `length(${column})` : column} >= ${checkLiteral(field.constraints.min)}`);
      if (field.constraints?.max !== undefined) checks.push(`${field.scalar === "string" ? `length(${column})` : column} <= ${checkLiteral(field.constraints.max)}`);
      if (field.constraints?.oneOf) checks.push(`${column} IN (${field.constraints.oneOf.map(checkLiteral).join(", ")})`);
      let reference = "";
      if (field.target !== undefined) {
        const target = this.table(field.target as string);
        reference = ` REFERENCES ${this.physical(target)} (${this.column(target, target.primaryKey)}) ON DELETE ${{ restrict: "RESTRICT", cascade: "CASCADE", setNull: "SET NULL" }[field.onDelete ?? "restrict"]} DEFERRABLE INITIALLY DEFERRED`;
      }
      return `${column} ${type}${field.allowsNull ? "" : " NOT NULL"}${name === table.primaryKey ? " PRIMARY KEY" : ""}${reference}${checks.map(check => ` CHECK (${check})`).join("")}`;
    });
    for (const check of table.checks) {
      let expression = check.parts[0]!;
      check.operands.forEach((operand, index) => {
        expression += ("field" in operand ? this.column(table, operand.field) : checkLiteral(operand.value)) + check.parts[index + 1]!;
      });
      definitions.push(`CONSTRAINT ${quote(check.name)} CHECK (${expression})`);
    }
    await this.execute(`CREATE TABLE ${this.physical(table)} (${definitions.join(", ")}) STRICT`);
    let index = 0;
    for (const unique of Object.values(table.unique)) {
      const fields = unique.fields.flatMap(name => {
        const column = this.column(table, name);
        return unique.nulls === "notDistinct" && table.fields[name]!.allowsNull ? [`(${column} IS NULL)`, `coalesce(${column}, '')`] : [column];
      });
      await this.execute(`CREATE UNIQUE INDEX ${quote(`_hulla_u${this.tables.indexOf(table)}_${index++}`)} ON ${this.physical(table)} (${fields.join(", ")})`);
    }
    for (const item of table.indexes) await this.execute(`CREATE INDEX ${quote(`_hulla_i${this.tables.indexOf(table)}_${index++}`)} ON ${this.physical(table)} (${item.fields.map(name => this.column(table, name)).join(", ")})`);
  }
  private predicate(table: CompiledTable, predicate: Predicate, parameters: SQLiteValue[]): string {
    if (predicate.kind === "constant") return predicate.value ? "1" : "0";
    if (predicate.kind === "and" || predicate.kind === "or") return predicate.predicates.length ? `(${predicate.predicates.map(item => this.predicate(table, item, parameters)).join(predicate.kind === "and" ? " AND " : " OR ")})` : predicate.kind === "and" ? "1" : "0";
    if (predicate.kind === "not") return `(NOT (${this.predicate(table, predicate.predicate, parameters)}))`;
    if (predicate.kind !== "compare") throw new TypeError("Invalid predicate");
    const path = typeof predicate.field === "string" ? [predicate.field] : predicate.field;
    const name = path[0]!;
    if (path.length !== 1 || predicate.transform)
      throw sqliteFailure("unsupported_query", "SQLite JSON predicates are not supported yet");
    const column = this.column(table, name);
    let operator = predicate.operator;
    if (operator === "is" || operator === "is not") return `${column} ${operator.toUpperCase()} NULL`;
    if (operator === "exists") return predicate.operand.kind === "value" && predicate.operand.value === true ? "1" : "0";
    if (table.fields[name]!.scalar === "json") throw sqliteFailure("unsupported_query", "SQLite JSON predicates are not supported yet");
    if (predicate.operand.kind === "field") {
      const other = typeof predicate.operand.field === "string" ? [predicate.operand.field] : predicate.operand.field;
      if (other.length !== 1 || predicate.operand.transform) throw sqliteFailure("unsupported_query", "SQLite JSON predicates are not supported yet");
      return `${column} ${operator} ${this.column(table, other[0]!)}`;
    }
    const value = predicate.operand.value;
    const bind = (value: unknown) => this.bind(table, name, value, parameters);
    if (operator === "in" || operator === "not in") {
      const values = value as unknown[];
      const nonNull = values.filter(item => item !== null);
      const matches: string[] = [];
      if (nonNull.length) matches.push(`${column} IN (${nonNull.map(bind).join(", ")})`);
      if (values.includes(null)) matches.push(`${column} IS NULL`);
      const result = matches.length ? `(${matches.join(" OR ")})` : "0";
      return operator === "not in" ? `(NOT ${result})` : result;
    }
    if (operator === "between" || operator === "not between") return `${column} ${operator.toUpperCase()} ${bind((value as unknown[])[0])} AND ${bind((value as unknown[])[1])}`;
    const negated = operator.startsWith("not ");
    if (negated) operator = operator.slice(4);
    if (["contains", "startsWith", "endsWith", "like", "ilike"].includes(operator)) {
      if (operator === "like" || operator === "ilike") {
        // SQLite LIKE is ASCII-insensitive by default. Require a deliberate supported operation.
        throw sqliteFailure("unsupported_query", "SQLite pattern matching is not supported; use literal text predicates");
      }
      const text = String(value), expression = column;
      if (predicate.caseSensitive === false) throw sqliteFailure("unsupported_query", "SQLite Unicode-insensitive text matching requires a platform extension and is not supported");
      parameters.push(text);
      const comparison = operator === "contains" ? `instr(${expression}, ?) > 0` : operator === "startsWith" ? `substr(${expression}, 1, length(?)) = ${bindText(text, parameters)}` : `substr(${expression}, length(${expression}) - length(?) + 1) = ${bindText(text, parameters)}`;
      return negated ? `(NOT (${comparison}))` : `(${comparison})`;
    }
    return `${column} ${operator} ${bind(value)}`;
  }
  where(table: CompiledTable, filters: Predicate[], parameters: SQLiteValue[]) {
    return filters.length ? filters.map(item => `(${this.predicate(table, item, parameters)})`).join(" AND ") : "1";
  }
  private order(table: CompiledTable, order: Sort[]) {
    const items = [...order];
    if (!items.some(item => item.field === table.primaryKey)) items.push({ field: table.primaryKey, direction: items.at(-1)?.direction ?? "asc" });
    return items;
  }
  async read(table: CompiledTable, filters: Predicate[], order: Sort[] = [], limit: number | null = null): Promise<StoredRow[]> {
    const parameters: SQLiteValue[] = [];
    const where = this.where(table, filters, parameters);
    const sort = this.order(table, order).map(item => `${this.column(table, item.field)} ${item.direction.toUpperCase()} NULLS ${item.direction === "asc" ? "LAST" : "FIRST"}`).join(", ");
    return (await this.execute(`SELECT ${this.fields(table)} FROM ${this.physical(table)} WHERE ${where} ORDER BY ${sort}${limit === null ? "" : ` LIMIT ${limit}`}`, parameters)).map(row => this.decode(table, row));
  }
  private expression(table: CompiledTable, name: string, node: WriteExpression, parameters: SQLiteValue[], incoming?: StoredRow): string {
    if (node.kind === "value") return this.bind(table, name, node.value, parameters);
    if (node.kind === "field") return node.source === "current" ? this.column(table, node.field) : this.bind(table, node.field, incoming![node.field], parameters);
    return `(${this.expression(table, name, node.left, parameters, incoming)} + ${this.expression(table, name, node.right, parameters, incoming)})`;
  }
  private async update(table: CompiledTable, values: Readonly<Record<string, WriteExpression>>, filters: Predicate[], incoming?: StoredRow) {
    const parameters: SQLiteValue[] = [];
    const assignments = Object.entries(values).map(([name, node]) => `${this.column(table, name)} = ${this.expression(table, name, node, parameters, incoming)}`);
    if (!assignments.length) assignments.push(`${this.column(table, table.primaryKey)} = ${this.column(table, table.primaryKey)}`);
    const where = this.where(table, filters, parameters);
    return (await this.execute(`UPDATE ${this.physical(table)} SET ${assignments.join(", ")} WHERE ${where} RETURNING ${this.fields(table)}`, parameters)).map(row => this.decode(table, row));
  }
  async operation(operation: Record<string, unknown>): Promise<unknown> {
    const table = this.table(operation.table as string);
    const filters = (operation.filters ?? []) as Predicate[];
    const projection = operation.projection as string[] | undefined;
    const envelopes = (rows: StoredRow[]) => rows.map(row => this.envelope(row, projection));
    if (operation.method === "select") return envelopes(await this.read(table, filters, operation.order as Sort[], operation.limit as number | null));
    if (operation.method === "paginate") throw sqliteFailure("unsupported_query", "SQLite pagination is not supported yet; use an explicit key boundary and limit");
    if (operation.method === "aggregate") return this.aggregate(table, operation, filters);
    if (operation.method === "update") return envelopes(await this.update(table, operation.values as Record<string, WriteExpression>, filters));
    if (operation.method === "delete") {
      const parameters: SQLiteValue[] = [];
      const where = this.where(table, filters, parameters);
      return envelopes((await this.execute(`DELETE FROM ${this.physical(table)} WHERE ${where} RETURNING ${this.fields(table)}`, parameters)).map(row => this.decode(table, row)));
    }
    if (operation.method !== "insert") throw new TypeError("Unknown SQLite operation");
    const bulk = Array.isArray(operation.values);
    const inputs = (bulk ? operation.values : [operation.values]) as StoredRow[];
    const conflict = operation.conflict as CompiledConflict | undefined;
    const touched = new Set<string>();
    const result: StoredRow[] = [];
    for (const input of inputs) {
      const row: StoredRow = { ...input };
      for (const [name, field] of Object.entries(table.fields)) {
        if (Object.hasOwn(row, name)) continue;
        if (field.hasDefault) { const value = field.encode(field.defaultValue); row[name] = typeof value === "bigint" ? String(value) : value; }
        else if (name === table.primaryKey) row[name] = uuid(table.keyGeneration === "uuidV4", this.randomBytes);
      }
      for (const [name, field] of Object.entries(table.fields)) field.decode(row[name]);
      if (conflict) {
        const nullsDistinct = conflict.fields.some(name => row[name] === null) && Object.values(table.unique).some(unique => unique.nulls === "distinct" && unique.fields.length === conflict.fields.length && unique.fields.every(name => conflict.fields.includes(name)));
        const matches = nullsDistinct ? [] : await this.read(table, conflict.fields.map(name => ({ kind: "compare", field: name, operator: row[name] === null ? "is" : "=", operand: { kind: "value", value: row[name] } })));
        if (matches.length) {
          if (conflict.action.kind === "skip") continue;
          const key = String(matches[0]![table.primaryKey]);
          if (touched.has(key)) throw sqliteFailure("cardinality_violation", "A bulk conflict update cannot update the same row twice");
          touched.add(key);
          result.push(...await this.update(table, conflict.action.values, [{ kind: "compare", field: table.primaryKey, operator: "=", operand: { kind: "value", value: matches[0]![table.primaryKey] } }], row));
          continue;
        }
      }
      const parameters: SQLiteValue[] = [];
      const names = Object.keys(table.fields);
      const values = names.map(name => this.bind(table, name, row[name], parameters));
      await this.execute(`INSERT INTO ${this.physical(table)} (${names.map(name => this.column(table, name)).join(", ")}) VALUES (${values.join(", ")})`, parameters);
      touched.add(String(row[table.primaryKey]));
      result.push(row);
    }
    return bulk ? envelopes(result) : result.length ? this.envelope(result[0]!, projection) : null;
  }
  private async aggregate(table: CompiledTable, operation: Record<string, unknown>, filters: Predicate[]) {
    const groups = operation.groupBy as string[];
    const aggregates = operation.aggregates as Record<string, AggregateDescriptor>;
    if (groups.length || (operation.having as Predicate[]).length) throw sqliteFailure("unsupported_query", "SQLite grouped aggregates are not supported yet");
    const columns = Object.entries(aggregates).map(([alias, item]) => {
      if (item.op === "count") return `count(*) AS ${quote(alias)}`;
      const field = table.fields[item.field!]!;
      if (field.scalar === "int64" && item.op === "avg") throw sqliteFailure("unsupported_query", "SQLite cannot preserve decimal int64 averages");
      const expression = `${item.op}(${this.column(table, item.field!)})`;
      return `${field.scalar === "int64" ? `CAST(${expression} AS TEXT)` : expression} AS ${quote(alias)}`;
    });
    const parameters: SQLiteValue[] = [];
    const where = this.where(table, filters, parameters);
    return (await this.execute(`SELECT ${columns.join(", ")} FROM ${this.physical(table)} WHERE ${where}`, parameters))[0];
  }
}

function bindText(text: string, parameters: SQLiteValue[]) { parameters.push(text); return "?"; }
function uuid(v4: boolean, randomBytes: (length: number) => Uint8Array) {
  const bytes = new Uint8Array(randomBytes(16));
  if (bytes.length !== 16) throw new TypeError("randomBytes must return the requested number of bytes");
  let timestamp = BigInt(Date.now());
  if (!v4) for (let index = 5; index >= 0; index--) { bytes[index] = Number(timestamp & 255n); timestamp >>= 8n; }
  bytes[6] = (bytes[6]! & 15) | (v4 ? 64 : 112); bytes[8] = (bytes[8]! & 63) | 128;
  const hex = Array.from(bytes, byte => byte.toString(16).padStart(2, "0")).join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}
