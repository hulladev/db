import { expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { resolve } from "node:path";
import { generateFunctions } from "../src/codegen.ts";
import { valueCodec, type ValueShape } from "../src/codec.ts";
import { executeDefinition } from "../src/declarations.ts";

test("generated Date contracts survive input stage copying and preserve result types at the browser boundary", async () => {
  const directory = await mkdtemp(new URL("./date-project-", import.meta.url).pathname);
  try {
    const source = resolve(directory, "dates.ts"), outDir = resolve(directory, "generated");
    await Bun.write(source, `import { defineFunctions } from '../../src/declarations.ts';
import { z } from 'zod';
const { query } = defineFunctions<{}, {}>();
export const echo = query({ input: z.object({ when: z.date() }), handler: (_ctx, input) => ({ dates: [input.when], label: 'literal' as Date | string }) });
`);
    await generateFunctions({ modules: { dates: source }, outDir });
    const manifest = await Bun.file(resolve(outDir, "functions.json")).json() as { functions: Record<string, { input: ValueShape; result: ValueShape }> };
    const contract = manifest.functions["dates.echo"]!;
    const input = valueCodec(contract.input), output = valueCodec(contract.result);
    const definition = (await import(source)).echo;
    const submitted = { when: new Date("2026-10-20T12:34:56.789Z") };
    const result = await executeDefinition(definition, {}, input.decode(input.encode(submitted)));
    const decoded = output.decode(output.encode(result)) as { dates: Date[]; label: Date | string };
    expect(decoded.dates[0]).toBeInstanceOf(Date);
    expect(decoded.dates[0]!.toISOString()).toBe("2026-10-20T12:34:56.789Z");
    expect(decoded.label).toBe("literal");
    expect(decoded.dates[0]).not.toBe(submitted.when);
  } finally { await rm(directory, { recursive: true, force: true }); }
});
