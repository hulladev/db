import {
  defineTable, replicaDefinition,
  type SchemaDefinition, type TableShape, type TableFields, type KeyName,
  type TableDefinition, type TableOptions,
} from "./schema.ts";
import { replicaSources, tableBindings, type ReplicaSource, type DatabaseIdentity } from "./database-metadata.ts";

declare const bindingType: unique symbol;
declare const replicaType: unique symbol;

export type ReplicaTable<D extends TableShape, Source extends DatabaseIdentity = DatabaseIdentity> = Omit<D, "index" | "extend" | "pick" | "omit"> & { readonly [replicaType]: Source };
export type ReplicaSourceOf<D> = D extends { readonly [replicaType]: infer Source extends DatabaseIdentity } ? Source : never;
type Projected<D extends TableShape, K extends keyof TableFields<D>> = TableDefinition<Pick<TableFields<D>, K>, D["options"]>;
type ReplicaDefinition<D extends TableShape> = TableDefinition<TableFields<D>, Omit<D["options"], "checks" | "unique"> & TableOptions>;
type Names<D extends TableShape> = Extract<keyof TableFields<D>, string>;
type Nonempty<K extends string> = readonly [K, ...K[]];
export interface BoundTableShape<S extends SchemaDefinition = SchemaDefinition, T extends string = string> {
  readonly [bindingType]: { readonly schema: S; readonly name: T };
}

/** A source-aware projection. Schema composition methods stay on standalone definitions. */
export type BoundTable<D extends TableShape = TableShape, S extends SchemaDefinition = SchemaDefinition, T extends string = string, Source extends DatabaseIdentity = DatabaseIdentity> =
  Omit<D, "index" | "extend" | "pick" | "omit"> & BoundTableShape<S, T> & {
    pick<const K extends Nonempty<Names<D>>>(...fields: K & (KeyName<D> extends K[number] ? unknown : { readonly missingPrimaryKey: KeyName<D> })): BoundTable<Projected<D, K[number]>, S, T, Source>;
    omit<const K extends Nonempty<Exclude<Names<D>, KeyName<D>>>>(...fields: K): BoundTable<Projected<D, Exclude<Names<D>, K[number]>>, S, T, Source>;
    replica(): ReplicaTable<ReplicaDefinition<D>, Source>;
  };

export type BoundName<B> = B extends { readonly [bindingType]: { readonly name: infer T } } ? T : never;

export function bindTable(source: Omit<ReplicaSource, "bound">): BoundTable {
  const { table, columns } = source;
  const shape = replicaDefinition(table, columns);
  const { index: _index, extend: _extend, pick: _pick, omit: _omit, ...metadata } = shape;
  const bound = Object.freeze({
    ...metadata,
    fields: Object.freeze(Object.fromEntries(columns.map(name => [name, source.definition.fields[name] ?? table.fields[name]!]))),
    options: source.definition.options,
    pick(...selected: string[]) { return project(selected, true); },
    omit(...selected: string[]) { return project(selected, false); },
    replica() {
      if (replicaSources.has(source.definition)) throw new TypeError("A replica must reference an authoritative table binding");
      const { index: _index, extend: _extend, pick: _pick, omit: _omit, ...metadata } = defineTable(shape.fields, shape.options);
      const replica = Object.freeze(metadata);
      replicaSources.set(replica, tableBindings.get(bound)!);
      return replica;
    },
  });
  tableBindings.set(bound, { ...source, bound });
  return bound as unknown as BoundTable;

  function project(selected: readonly string[], include: boolean): BoundTable {
    if (!selected.length || new Set(selected).size !== selected.length) throw new TypeError("Table projection requires distinct fields");
    for (const name of selected) if (typeof name !== "string" || !columns.includes(name)) throw new TypeError(`Unknown projection field ${String(name)}`);
    const projected = include ? [...selected] : columns.filter(name => !selected.includes(name));
    if (!projected.includes(table.primaryKey)) throw new TypeError(`Replica projections must include primary key ${table.primaryKey}`);
    return bindTable({ ...source, columns: Object.freeze(projected) });
  }
}
