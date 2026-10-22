import type { BetterAuthOptions } from "better-auth";
import { createAdapterFactory, type AdapterFactory, type CustomAdapter } from "better-auth/adapters";
import type { SchemaDefinition } from "@hulla/db/schema";
import type { Database, Expressions, PendingWrite, Predicate, SelectQuery } from "@hulla/db/server";
import { generateAuthSchema } from "./schema.ts";

// Better Auth discovers plugin models at runtime. Erasure stays at this boundary;
// the real database still validates every model, column, predicate and value.
type AuthRow = Record<string, any>;
interface Store {
  select(model: string, projection?: readonly string[]): SelectQuery<AuthRow>;
  insert(model: string, data: AuthRow): PromiseLike<AuthRow>;
  update(model: string, data: AuthRow): PendingWrite<AuthRow>;
  delete(model: string): PendingWrite<AuthRow>;
}
type Conditions = Parameters<CustomAdapter["update"]>[0]["where"];

function filter(where: Conditions = []): (q: Expressions<AuthRow, string>) => Predicate {
  return q => {
    const exact = (field: string, value: string) => q.ilike(field, value.replace(/[\\%_]/g, "\\$&"));
    const condition = (w: Conditions[number]): Predicate => {
      const { field, value, operator, mode } = w;
      const insensitive = mode === "insensitive";
      switch (operator) {
        case "eq": return insensitive && typeof value === "string" ? exact(field, value) : q.eq(field, value);
        case "ne": return insensitive && typeof value === "string" ? q.not(exact(field, value)) : q.ne(field, value);
        case "lt": return q.lt(field, value as string | number | Date);
        case "lte": return q.lte(field, value as string | number | Date);
        case "gt": return q.gt(field, value as string | number | Date);
        case "gte": return q.gte(field, value as string | number | Date);
        case "in":
        case "not_in": {
          if (!Array.isArray(value)) throw new TypeError(`Better Auth ${operator} requires an array`);
          const matched = insensitive && value.every(item => typeof item === "string")
            ? q.or(...(value as string[]).map(item => exact(field, item)))
            : q.inArray(field, value);
          return operator === "not_in" ? q.not(matched) : matched;
        }
        case "contains": return q.contains(field, value as string, { caseSensitive: !insensitive });
        case "starts_with": return q.startsWith(field, value as string, { caseSensitive: !insensitive });
        case "ends_with": return q.endsWith(field, value as string, { caseSensitive: !insensitive });
        default: throw new TypeError(`Unsupported Better Auth operator: ${String(operator)}`);
      }
    };
    // Match Better Auth's adapter convention: AND predicates constrain the
    // complete OR group, rather than interpreting the array left to right.
    const and = where.filter(w => w.connector !== "OR").map(condition);
    const or = where.filter(w => w.connector === "OR").map(condition);
    return q.and(...and, ...(or.length ? [q.or(...or)] : []));
  };
}

/** Store Better Auth records in the application's generated server database. */
export function hullaAdapter<S extends SchemaDefinition>(db: Database<S>): AdapterFactory<BetterAuthOptions> {
  return (options: BetterAuthOptions) => {
    if (options.advanced?.database?.joins) throw new TypeError("Hulla uses Better Auth's portable join fallback; disable advanced.database.joins");

    function adapter(database: Store, inTransaction: boolean): ReturnType<AdapterFactory<BetterAuthOptions>> {
      const atomic = <T>(callback: (tx: Store) => Promise<T>): Promise<T> => inTransaction
        ? callback(database)
        // Only these internal DB-only callbacks can retry after a confirmed OCC
        // rollback. Better Auth's application transaction callbacks run once.
        : db.transaction(tx => callback(tx as unknown as Store), { retries: 3 });
      const first = async (tx: Store, model: string, where: Conditions) =>
        (await tx.select(model).where(filter(where)).limit(1))[0] ?? null;

      return createAdapterFactory({
        config: {
          adapterId: "hulla", adapterName: "Hulla DB",
          supportsDates: true, supportsBooleans: true, supportsJSON: true,
          supportsArrays: true, supportsNumericIds: false, supportsUUIDs: false,
          transaction: inTransaction ? false : callback => db.transaction(tx => callback(adapter(tx as unknown as Store, true))),
        },
        adapter: ({ getFieldName }) => ({
          async create({ model, data }) { return await database.insert(model, data) as typeof data; },
          async findOne<T>({ model, modelKey, where, select }: Parameters<CustomAdapter["findOne"]>[0]) {
            const projection = select?.length ? select.map(field => getFieldName({ model: modelKey ?? model, field })) : undefined;
            return (await database.select(model, projection).where(filter(where)).limit(1))[0] as T ?? null;
          },
          async findMany<T>({ model, modelKey, where, limit, offset = 0, select, sortBy }: Parameters<CustomAdapter["findMany"]>[0]) {
            if (!Number.isSafeInteger(limit) || limit < 0 || !Number.isSafeInteger(offset) || offset < 0 || !Number.isSafeInteger(offset + limit)) {
              throw new TypeError("Better Auth limit/offset must be nonnegative safe integers");
            }
            if (limit === 0) return [];
            const projection = select?.length ? select.map(field => getFieldName({ model: modelKey ?? model, field })) : undefined;
            let query = database.select(model, projection).where(filter(where));
            if (sortBy) query = query.orderBy(getFieldName({ model: modelKey ?? model, field: sortBy.field }), sortBy.direction);
            // Core exposes cursor pagination. Better Auth requires offset; read a
            // bounded ordered prefix under the normal database execution limits.
            return (await query.limit(offset + limit)).slice(offset) as T[];
          },
          async update<T>({ model, where, update }: Parameters<CustomAdapter["update"]>[0]) {
            return atomic(async tx => {
              const row = await first(tx, model, where);
              if (!row) return null;
              return (await tx.update(model, update as AuthRow).where({ id: row.id }))[0] as T ?? null;
            });
          },
          async updateMany({ model, where, update }) {
            return (await database.update(model, update).where(filter(where))).length;
          },
          async delete({ model, where }) {
            await atomic(async tx => {
              const row = await first(tx, model, where);
              if (row) await tx.delete(model).where({ id: row.id });
            });
          },
          async deleteMany({ model, where }) { return (await database.delete(model).where(filter(where))).length; },
          async consumeOne<T>({ model, where }: Parameters<NonNullable<CustomAdapter["consumeOne"]>>[0]) {
            return atomic(async tx => {
              const row = await first(tx, model, where);
              return row ? (await tx.delete(model).where({ id: row.id }))[0] as T ?? null : null;
            });
          },
          async count({ model, where }) {
            const result = await database.select(model).where(filter(where)).aggregate(({ count }) => ({ count: count() }));
            if (!Number.isSafeInteger(result.count)) throw new RangeError("Better Auth count exceeds the safe integer range");
            return result.count;
          },
          async createSchema({ file, tables }) {
            return { code: generateAuthSchema(tables), path: file ?? "./db/auth-schema.ts", overwrite: false };
          },
        } satisfies CustomAdapter),
      })(options);
    }
    return adapter(db as unknown as Store, false);
  };
}
