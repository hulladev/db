import { expect, test } from "bun:test";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { runInNewContext } from "node:vm";
import { compileWorker } from "../src/worker-codegen.ts";

// Bundle execution here protects library interoperability. Native integration
// tests separately enforce the capability boundary and transaction guarantees.
test("restricted bundles preserve real Zod and Valibot namespace reexports and async validation", async () => {
  const root = await mkdtemp(new URL("./validator-bundle-", import.meta.url).pathname);
  try {
    const entry = resolve(root, "worker.ts");
    await writeFile(entry, `import { z } from 'zod';
import * as v from 'valibot';
const input = z.object({ title: z.string().transform(async title => { await Promise.resolve(); return title.trim(); }) });
const output = z.object({ title: z.string() }).transform(async value => { await Promise.resolve(); return { label: value.title.toUpperCase() }; });
const alternate = v.pipeAsync(v.object({ title: v.string() }), v.transformAsync(async value => ({ title: value.title.trim() })));
globalThis.validate = async value => {
  const parsed = await input['~standard'].validate(value);
  if (parsed.issues) return 'invalid';
  const checked = await alternate['~standard'].validate(parsed.value);
  if (checked.issues) return 'invalid';
  return await output['~standard'].validate(checked.value);
};`);
    const source = await compileWorker(entry);
    const globals: { validate?: (value: unknown) => Promise<unknown> } = {};
    runInNewContext(source, globals);
    expect(await globals.validate!({ title: "  bundled validators  " })).toEqual({ value: { label: "BUNDLED VALIDATORS" } });
    expect(await globals.validate!({ title: 123 })).toBe("invalid");
  } finally { await rm(root, { recursive: true, force: true }); }
});

test("restricted bundling rejects host imports and statically resolved dynamic imports", async () => {
  const root = await mkdtemp(new URL("./forbidden-bundle-", import.meta.url).pathname);
  try {
    await writeFile(resolve(root, "helper.ts"), "export const value = 1;");
    for (const body of [
      "import { readFileSync } from 'node:fs'; globalThis.result = readFileSync('secret');",
      "import { readFileSync } from 'fs'; globalThis.result = readFileSync('secret');",
      "globalThis.run = async () => await import('./helper.ts');",
    ]) {
      const entry = resolve(root, "worker.ts");
      await writeFile(entry, body);
      await expect(compileWorker(entry)).rejects.toThrow("Unsupported declaration import");
    }
  } finally { await rm(root, { recursive: true, force: true }); }
});
