import type { BetterAuthOptions } from "better-auth";
import { getAuthTables, type BetterAuthDBSchema, type DBFieldAttribute } from "@better-auth/core/db";
import { defineTable, v, type TableDefinition, type TableOptions } from "@hulla/db/schema";

type AuthField = TableDefinition["fields"][string];
type AuthTable = TableDefinition<Record<string, AuthField>, { primaryKey: "id"; keyGeneration: "none"; unique: NonNullable<TableOptions["unique"]> }>;
type StringField = ReturnType<typeof v.string>;
type DateField = ReturnType<typeof v.timestamp<{}>>;
type NullableString = ReturnType<ReturnType<StringField["nullable"]>["default"]>;
type NullableDate = ReturnType<ReturnType<DateField["nullable"]>["default"]>;
type CoreTable<F extends Record<string, AuthField>> = TableDefinition<F & { id: StringField }, { primaryKey: "id"; keyGeneration: "none" }>;

/** The default core schema has known names. Configured schemas are discovered at runtime. */
export type BetterAuthCoreTables = {
  user: CoreTable<{ name: StringField; email: StringField; emailVerified: ReturnType<ReturnType<typeof v.boolean>["default"]>; image: NullableString; createdAt: DateField; updatedAt: DateField }>;
  session: CoreTable<{ expiresAt: DateField; token: StringField; createdAt: DateField; updatedAt: DateField; ipAddress: NullableString; userAgent: NullableString; userId: StringField }>;
  account: CoreTable<{ accountId: StringField; providerId: StringField; userId: StringField; accessToken: NullableString; refreshToken: NullableString; idToken: NullableString; accessTokenExpiresAt: NullableDate; refreshTokenExpiresAt: NullableDate; scope: NullableString; password: NullableString; createdAt: DateField; updatedAt: DateField }>;
  verification: CoreTable<{ identifier: StringField; value: StringField; expiresAt: DateField; createdAt: DateField; updatedAt: DateField }>;
};

interface FieldDescription { field: AuthField; code: string }
interface TableDescription {
  name: string;
  fields: Record<string, FieldDescription>;
  unique: NonNullable<TableOptions["unique"]>;
  indexes: { fields: string[]; name?: string }[];
}

function unsupported(path: string, detail: string): never {
  throw new TypeError(`Better Auth schema ${path}: ${detail}`);
}

function literal(value: unknown, path: string): string {
  if (value instanceof Date) {
    if (!Number.isFinite(value.getTime())) unsupported(path, "invalid Date default");
    return `new Date(${JSON.stringify(value.toISOString())})`;
  }
  // Schema defaults must be JSON values or Dates, with no lossy serialization.
  if (value === null || typeof value === "string" || typeof value === "boolean") return JSON.stringify(value);
  if (typeof value === "number" && Number.isFinite(value)) return Object.is(value, -0) ? "-0" : String(value);
  if (Array.isArray(value)) return `[${value.map(item => literal(item, path)).join(", ")}]`;
  if (typeof value === "object" && value !== null && Object.getPrototypeOf(value) === Object.prototype) {
    return `{ ${Object.entries(value).map(([key, item]) => `[${JSON.stringify(key)}]: ${literal(item, path)}`).join(", ")} }`;
  }
  return unsupported(path, "unsupported literal default");
}

function describeField(attribute: DBFieldAttribute, path: string, tables: BetterAuthDBSchema): FieldDescription {
  let field: AuthField;
  let code: string;
  const reference = attribute.references;
  if (reference) {
    const target = tables[reference.model] ?? Object.values(tables).find(table => table.modelName === reference.model);
    if (!target) unsupported(path, `unknown reference model ${reference.model}`);
    if (reference.field !== "id") unsupported(path, "references must target the model's id primary key");
    if (attribute.type !== "string") unsupported(path, "references require string IDs");
    const action = reference.onDelete ?? "cascade";
    if (action !== "cascade" && action !== "restrict" && action !== "set null") unsupported(path, `unsupported onDelete action ${action}`);
    if (action === "set null" && attribute.required !== false) unsupported(path, "set null references must be optional");
    const onDelete = action === "set null" ? "setNull" : action;
    field = v.id(target.modelName, { onDelete });
    code = `v.id(${JSON.stringify(target.modelName)}, { onDelete: ${JSON.stringify(onDelete)} })`;
  } else if (Array.isArray(attribute.type)) {
    if (!attribute.type.length || attribute.type.some(value => typeof value !== "string")) unsupported(path, "enum fields need a nonempty list of strings");
    field = v.string().oneOf(attribute.type);
    code = `v.string().oneOf(${JSON.stringify(attribute.type)})`;
  } else {
    switch (attribute.type) {
      case "string": field = v.string(); code = "v.string()"; break;
      case "number": field = v.int64(); code = "v.int64()"; break;
      case "boolean": field = v.boolean(); code = "v.boolean()"; break;
      case "date": field = v.timestamp(); code = "v.timestamp()"; break;
      case "json": field = v.json(); code = "v.json()"; break;
      case "string[]": field = v.json(v.array(v.string())); code = "v.json(v.array(v.string()))"; break;
      case "number[]": field = v.json(v.array(v.number())); code = "v.json(v.array(v.number()))"; break;
      default: return unsupported(path, `unsupported field type ${String(attribute.type)}`);
    }
  }
  if (attribute.required === false) { field = field.nullable(); code += ".nullable()"; }
  // Better Auth evaluates function defaults, transforms and onUpdate itself.
  // A nullable database field may be omitted when inserting through Hulla too.
  const value = typeof attribute.defaultValue === "function" ? undefined : attribute.defaultValue;
  if (value !== undefined) {
    code += `.default(${literal(value, path)})`;
    field = field.default(value);
  } else if (attribute.required === false) { field = field.default(null); code += ".default(null)"; }
  return { field, code };
}

function describeTables(tables: BetterAuthDBSchema): TableDescription[] {
  const names = new Set<string>();
  return Object.entries(tables).map(([logicalName, table], tableNumber) => {
    if (table.disableMigrations) unsupported(logicalName, "disableMigrations cannot be honored by a Hulla-managed schema");
    if (!table.modelName || names.has(table.modelName)) unsupported(logicalName, "modelName must be nonempty and distinct");
    names.add(table.modelName);
    const fields: Record<string, FieldDescription> = Object.create(null);
    fields.id = { field: v.string(), code: "v.string()" };
    const unique: Record<string, { fields: readonly string[]; nulls: "distinct" }> = Object.create(null);
    const indexes: TableDescription["indexes"] = [];
    const uniqueSignatures = new Set<string>();
    const indexSignatures = new Set<string>();
    const indexNames = new Set<string>();
    const physical = (key: string): string => {
      if (key === "id") return "id";
      const field = table.fields[key];
      if (!field) return unsupported(`${logicalName}.${key}`, "unknown index field");
      return field.fieldName || key;
    };
    const addUnique = (selected: string[], name?: string) => {
      const signature = JSON.stringify([...selected].sort());
      if (uniqueSignatures.has(signature)) return;
      uniqueSignatures.add(signature);
      const constraintName = name ?? `ba_${tableNumber}_unique_${uniqueSignatures.size}`;
      if (Object.hasOwn(unique, constraintName)) unsupported(logicalName, `duplicate unique name ${constraintName}`);
      unique[constraintName] = { fields: selected, nulls: "distinct" };
    };
    const addIndex = (selected: string[], name?: string) => {
      const signature = JSON.stringify(selected);
      if (indexSignatures.has(signature)) return;
      if (name && indexNames.has(name)) unsupported(logicalName, `duplicate index name ${name}`);
      indexSignatures.add(signature);
      if (name) indexNames.add(name);
      indexes.push({ fields: selected, ...(name === undefined ? {} : { name }) });
    };
    for (const [key, attribute] of Object.entries(table.fields)) {
      const fieldName = attribute.fieldName || key;
      if (Object.hasOwn(fields, fieldName)) unsupported(`${logicalName}.${key}`, `duplicate or reserved column ${fieldName}`);
      fields[fieldName] = describeField(attribute, `${logicalName}.${key}`, tables);
      if (attribute.unique) addUnique([fieldName]);
      else if (attribute.index) addIndex([fieldName]);
    }
    for (const index of table.indexes ?? []) {
      if (!index.fields.length || new Set(index.fields).size !== index.fields.length) unsupported(logicalName, "indexes require distinct fields");
      const selected = index.fields.map(physical);
      if (new Set(selected).size !== selected.length) unsupported(logicalName, "index fields resolve to the same database column");
      if (index.unique) addUnique(selected, index.name);
      else addIndex(selected, index.name);
    }
    return { name: table.modelName, fields, unique, indexes };
  });
}

/** Produce tables with Better Auth's physical model and column names. */
export function betterAuthTables(): BetterAuthCoreTables;
export function betterAuthTables(options: BetterAuthOptions): Record<string, AuthTable>;
export function betterAuthTables(options: BetterAuthOptions = {}): BetterAuthCoreTables | Record<string, AuthTable> {
  if (options.advanced?.database?.generateId === "serial") unsupported("advanced.database.generateId", "serial numeric IDs are unsupported; use Better Auth string IDs");
  return Object.fromEntries(describeTables(getAuthTables(options)).map(table => {
    let definition = defineTable(Object.fromEntries(Object.entries(table.fields).map(([key, value]) => [key, value.field])), { primaryKey: "id", keyGeneration: "none", unique: table.unique });
    for (const index of table.indexes) definition = definition.index(index.fields, index.name === undefined ? {} : { name: index.name });
    return [table.name, definition];
  }));
}

/** Generate an independent schema module; it can be imported before the application database exists. */
export function generateAuthSchema(tables: BetterAuthDBSchema): string {
  const declarations = describeTables(tables).map(table => {
    const fields = Object.entries(table.fields).map(([key, value]) => `    [${JSON.stringify(key)}]: ${value.code},`).join("\n");
    const options = `{ primaryKey: "id", keyGeneration: "none", unique: ${JSON.stringify(table.unique)} }`;
    const indexes = table.indexes.map(index => `.index(${JSON.stringify(index.fields)}${index.name === undefined ? "" : `, { name: ${JSON.stringify(index.name)} }`})`).join("");
    return `  [${JSON.stringify(table.name)}]: defineTable({\n${fields}\n  }, ${options})${indexes},`;
  }).join("\n");
  return `// Generated from Better Auth's schema. Better Auth owns IDs and function defaults.\nimport { defineTable, v } from "@hulla/db/schema";\n\nexport const authTables = {\n${declarations}\n};\n\nexport default authTables;\n`;
}
