import { generateFunctions, generateSchema } from "../src/codegen.ts";
import { compileWorker } from "../src/worker-codegen.ts";

await generateSchema({
  schema: new URL("./fixtures/schema.ts", import.meta.url).pathname,
  outDir: new URL("./generated", import.meta.url).pathname,
  rustOutput: new URL(
    "../../../crates/hulladb_schema/tests/fixtures/articles_schema.rs",
    import.meta.url,
  ).pathname,
});

await generateSchema({
  schema: new URL("./fixtures/deployed-schema.ts", import.meta.url).pathname,
  outDir: new URL("./generated/deployed", import.meta.url).pathname,
  rustOutput: new URL("../../../crates/hulladb_server/tests/fixtures/deployed_schema.rs", import.meta.url).pathname,
});

await generateFunctions({
  modules: {
    deployed: new URL("./fixtures/deployed.ts", import.meta.url).pathname,
    typescript: new URL("./fixtures/typescript.ts", import.meta.url).pathname,
    notes: new URL("./fixtures/notes.ts", import.meta.url).pathname,
    names: new URL("./fixtures/names.ts", import.meta.url).pathname,
  },
  outDir: new URL("./generated", import.meta.url).pathname,
});

for (const name of ["deployed", "schema"] as const) {
  const source = new URL(`./fixtures/${name}-worker.ts`, import.meta.url).pathname;
  await Bun.write(new URL(`./generated/${name}-worker.js`, import.meta.url), await compileWorker(source));
}
