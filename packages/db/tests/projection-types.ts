import type { WriteDatabase } from "../src/database.ts";
import { defineSchema, defineTable, v } from "../src/schema.ts";
import { createTables } from "../src/fields.ts";

const schema = defineSchema({ tasks: defineTable({ title: v.string(), done: v.boolean(), rank: v.int64({ mode: "bigint" }).nullable() }) });
declare const db: WriteDatabase<typeof schema>;
declare const include: boolean;
const { tasks } = createTables<typeof schema>({ tasks: { id: {}, title: {}, done: {}, rank: {} } });
const array = db.select(tasks, ["title", include && "rank", false, null, undefined])
  .where("done", "=", true).orderBy(tasks.id).limit(1);
const object = db.select("tasks", { title: true, rank: include, done: false, id: undefined }).unique();
const inserted = db.insert(tasks, { title: "draft", done: false, rank: null }).returning(["title"]);
const batch = db.insert(tasks, []).returning({ title: true, rank: include });
const updated = db.update(tasks, { title: "new" }).returning(["title"]).where("rank", ">", 0n);
const deleted = db.delete(tasks).where({ done: true }).returning({ rank: true });
const paged = db.select(tasks, ["title"]).orderBy("rank").paginate({ limit: 1 });

type Assert<T extends true> = T;
type Equal<A, B> = (<T>() => T extends A ? 1 : 2) extends (<T>() => T extends B ? 1 : 2) ? true : false;
type Normalize<T> = { [K in keyof T]: T[K] };
export type ArrayProjection = Assert<Equal<Normalize<Awaited<typeof array>[number]>, { title: string; rank?: bigint | null }>>;
export type ObjectProjection = Assert<Equal<Normalize<NonNullable<Awaited<typeof object>>>, { title: string; rank?: bigint | null }>>;
export type SingleInsert = Assert<Equal<Normalize<Awaited<typeof inserted>>, { title: string }>>;
export type BulkInsert = Assert<Equal<Normalize<Awaited<typeof batch>[number]>, { title: string; rank?: bigint | null }>>;
export type UpdateProjection = Assert<Equal<Normalize<Awaited<typeof updated>[number]>, { title: string }>>;
export type DeleteProjection = Assert<Equal<Normalize<Awaited<typeof deleted>[number]>, { rank: bigint | null }>>;
export type PageProjection = Assert<Equal<Normalize<Awaited<typeof paged>["page"][number]>, { title: string }>>;

declare const alternative: "title" | "done";
const uncertain = db.select(tasks, [alternative]);
export type UnionSelection = Assert<Equal<Normalize<Awaited<typeof uncertain>[number]>, { title?: string; done?: boolean }>>;
declare const dynamic: ("title" | "rank")[];
const runtimeArray = db.select(tasks, dynamic);
export type RuntimeSelection = Assert<Equal<Normalize<Awaited<typeof runtimeArray>[number]>, { title?: string; rank?: bigint | null }>>;
// @ts-expect-error missing columns cannot be selected
db.select(tasks, ["missing"]);
// @ts-expect-error unknown object keys remain invalid even when excluded
db.select(tasks, { title: true, missing: false });
// @ts-expect-error object projection supports inclusion flags only
db.select(tasks, { title: "name" });
// @ts-expect-error arbitrary falsy entries are not conditional selection
db.select(tasks, ["title", 0]);
// @ts-expect-error projection does not widen filter value types
db.select(tasks, ["title"]).where("rank", ">", "wrong");
// @ts-expect-error write projections have the same key validation
db.insert(tasks, []).returning({ title: true, missing: false });
// @ts-expect-error returning does not make an unscoped write executable
db.delete(tasks).returning(["title"]).then(() => {});
