import { expect, test } from "bun:test";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import { resolve } from "node:path";
import { runInNewContext } from "node:vm";
import { API } from "typescript/unstable/async";
import type { SchemaDefinition } from "@hulla/db/schema";
import { generateProject } from "../../db/src/project-codegen.ts";
import { schemaDescriptor } from "../../db/src/schema-contract.ts";

test("Better Auth setup bootstraps generation, infers auth rows and executes the restricted worker", async () => {
  const root = await mkdtemp(new URL("./auth-setup-", import.meta.url).pathname);
  try {
    await mkdir(resolve(root, "db"));
    await Bun.write(resolve(root, "tsconfig.json"), JSON.stringify({ extends: "../../tsconfig.json", include: ["./**/*.ts"] }));
    await Bun.write(resolve(root, "db/schema.ts"), `import { defineSchema, defineTable, v } from '@hulla/db/schema';
import { betterAuthTables } from '@hulla/db-better-auth/schema';
export default defineSchema({ ...betterAuthTables(), tasks: defineTable({ title: v.string() }) });
`);
    const config = resolve(root, "db.config.ts");
    await Bun.write(config, `import { defineConfig } from '@hulla/db/config';
export default defineConfig({
  database: { server: { url: 'postgresql://app:secret@127.0.0.1:1/app' } },
  auth: async (headers: Headers) => {
    const { auth } = await import('./auth.ts');
    const session = await auth.api.getSession({ headers });
    return session ? { key: session.user.id, identity: session.user } : null;
  },
});
`);
    await Bun.write(resolve(root, "auth.ts"), `import { betterAuth } from 'better-auth';
import { hullaAdapter } from '@hulla/db-better-auth';
import { db } from './db/_generated/server.ts';
export const auth = betterAuth({
  secret: 'test-only-better-auth-secret-at-least-32-characters',
  database: hullaAdapter(db),
  emailAndPassword: { enabled: true },
});
`);
    // The config's lazy auth import must permit this first run before server.ts exists.
    await generateProject(config);
    const declarations = resolve(root, "db/tasks.ts");
    await Bun.write(declarations, `import { query } from './_generated/functions.ts';
export const who = query(ctx => ctx.identity?.id ?? null);
export const created = query(async ctx => {
  const users = await ctx.db.select('user').limit(1);
  const createdAt: Date | null = users[0]?.createdAt ?? null;
  return createdAt;
});
`);
    await generateProject(config);
    const proof = resolve(root, "types-proof.ts");
    await Bun.write(proof, `import type { Schema } from './db/_generated/types.ts';
import type { Insert, Row } from '@hulla/db/schema';
declare const user: Row<Schema, 'user'>;
const created: Date = user.createdAt;
const email: string = user.email;
// @ts-expect-error optional images remain nullable
const image: string = user.image;
// @ts-expect-error Better Auth owns opaque string IDs
const numericId: number = user.id;
const insert: Insert<Schema, 'user'> = { id: 'opaque-user', name: 'Alice', email: 'alice@example.com', createdAt: new Date(), updatedAt: new Date() };
void [created, email, image, numericId, insert];
`);
    const api = new API();
    try {
      const paths = [config, resolve(root, "auth.ts"), resolve(root, "db/schema.ts"), declarations, proof];
      const snapshot = await api.updateSnapshot({ openFiles: paths });
      try {
        for (const path of paths) {
          const project = await snapshot.getDefaultProjectForFile(path);
          expect(project).toBeDefined();
          expect(await project!.program.getSemanticDiagnostics(path)).toEqual([]);
        }
      } finally { await snapshot.dispose(); }
    } finally { await api.close(); }

    const globals: { __hullaExecute?: (invocation: unknown) => Promise<string>; __hullaDb: () => Promise<string>; TextEncoder: typeof TextEncoder } = {
      TextEncoder,
      __hullaDb: async () => { throw new Error("Identity query must not access database storage"); },
    };
    runInNewContext(await Bun.file(resolve(root, "db/_generated/worker.js")).text(), globals);
    const schema = (await import(resolve(root, "db/schema.ts")) as { default: SchemaDefinition }).default;
    const invoke = (identity: unknown) => globals.__hullaExecute!({
      schema: schemaDescriptor(schema), name: "tasks.who", kind: "query", identity,
      input_shape: { kind: "undefined" }, input: null,
      result_shape: { kind: "union", choices: [{ kind: "string" }, { kind: "null" }] },
    });
    expect(JSON.parse(await invoke({ id: "opaque-user" }))).toBe("opaque-user");
    expect(JSON.parse(await invoke(null))).toBeNull();
  } finally { await rm(root, { recursive: true, force: true }); }
}, 30_000);
