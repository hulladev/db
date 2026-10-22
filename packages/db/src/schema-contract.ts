import { compileSchema, type SchemaDefinition, type Scalar } from "./schema.ts";

const scalars: Record<Scalar, string> = { string: "String", boolean: "Boolean", int64: "Int64", uuid: "Uuid", float64: "Float64", json: "Json", date: "Date", timestamp: "Timestamp", localTimestamp: "LocalTimestamp" };

export function scalarDescriptor(scalar: Scalar, value: unknown): unknown {
  return value === null ? ["null"] : [scalar, typeof value === "bigint" ? String(value) : value];
}

/** Exact logical contract used by the native installation/worker equality guard. */
export function schemaDescriptor(schema: SchemaDefinition): unknown {
  const tables = compileSchema(schema);
  return {
    uuid_default: schema.defaultUuidGeneration === "v4" ? "V4" : "V7",
    tables: tables.map(table => ({
      name: table.name, key: table.primaryKey,
      generation: typeof table.keyGeneration === "object"
        ? { custom: table.keyGeneration.handler, output: scalars[table.keyGeneration.outputType] }
        : { none: "None", uuidV4: "UuidV4", uuidV7: "UuidV7" }[table.keyGeneration],
      insert_policy: table.insertKeyPolicy === "generatedOnly" ? "GeneratedOnly" : "AllowExplicit",
      fields: Object.entries(table.fields).map(([name, field]) => {
        const value = field.hasDefault ? field.encode(field.defaultValue) : undefined;
        return {
          name, scalar: scalars[field.scalar], nullable: field.allowsNull,
          ...(field.jsonShape ? { jsonShape: field.jsonShape } : {}),
          ...(field.precision === undefined ? {} : { precision: field.precision }),
          default: !field.hasDefault ? null : scalarDescriptor(field.scalar, value),
          constraints: {
            ...(field.constraints?.min === undefined ? {} : { min: scalarDescriptor(field.scalar === "string" ? "int64" : field.scalar, field.scalar === "string" ? BigInt(field.constraints.min) : field.constraints.min) }),
            ...(field.constraints?.max === undefined ? {} : { max: scalarDescriptor(field.scalar === "string" ? "int64" : field.scalar, field.scalar === "string" ? BigInt(field.constraints.max) : field.constraints.max) }),
            oneOf: (field.constraints?.oneOf ?? []).map(value => scalarDescriptor(field.scalar, value)),
          },
        };
      }),
      unique: Object.entries(table.unique).map(([name, unique]) => ({
        name, fields: unique.fields, nulls: unique.nulls === "distinct" ? "Distinct" : "NotDistinct",
      })),
      indexes: table.indexes,
      checks: table.checks.map(check => ({ name: check.name, parts: check.parts, operands: check.operands.map(operand =>
        "field" in operand ? operand : { value: scalarDescriptor(typeof operand.value === "bigint" ? "int64" : typeof operand.value === "number" ? "float64" : typeof operand.value === "boolean" ? "boolean" : "string", operand.value) }),
      })),
    })),
    references: tables.flatMap(table => Object.entries(table.fields).flatMap(([name, field]) =>
      field.target === undefined ? [] : [{ table: table.name, field: name, target: field.target,
        key: tables.find(table => table.name === field.target)!.primaryKey,
        onDelete: { restrict: "Restrict", cascade: "Cascade", setNull: "SetNull" }[field.onDelete ?? "restrict"] }],
    )),
  };
}
