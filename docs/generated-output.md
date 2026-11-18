# Generated output and project tooling

`hulla-db generate` owns the `_generated` directory beside the configured schema. Generation never
edits formatter configuration or ignore files. Keep project-wide formatting and linting away from
managed output, especially its bundled worker runtime.

## Explicit ignore setup

The `schema` value in `db.config.ts` determines the output directory: `_generated` beside that
schema. Use a positional config path to select another config. `--ignore-file` is a one-time setup
input, not a persisted formatter choice; the resulting entry belongs to the selected tool's config.

Choose a gitignore-syntax file that your tools read, then run setup once from the app directory:

```sh
bun hulla-db setup --ignore-file '<ignore-file-path>'
bun hulla-db generate
```

Setup reads `db.config.ts` to resolve the output directory, appends its anchored path relative to the
selected ignore file, and preserves existing content and newline style. Repeating setup does not add
another entry. It does not generate code, open a database or start a service. Custom config paths work
as in other commands: `hulla-db setup ./config/db.config.ts --ignore-file .prettierignore`.

`.prettierignore` is supported by Prettier and Oxfmt. The file must be in an ancestor of the generated
directory. The selected file must use gitignore syntax, not JSON, JavaScript or TypeScript configuration.
Select it only when your formatter actually reads that file; a custom `--ignore-path` can change the
active ignore files. Explicit file arguments can also behave differently across tools.

If the project keeps ignores inside tool configuration, add the resolved directory there instead:

```json
{
  "ignorePatterns": ["<generated-output-path>/**"]
}
```

For Vite+, its workspace `fmt.ignorePatterns` belongs in `vite.config.ts`. Linting has its own
configuration; exclude the generated directory there too. Setup does not infer tools, edit source
configuration files or change which generated artifacts your project commits.

In `biome.json` or `biome.jsonc`, extend `files.includes` with a directory exclusion. Preserve the
project's existing positive includes; this example starts with all files:

```json
{
  "files": {
    "includes": ["**", "!**/_generated"]
  }
}
```

This excludes generated output from formatting and linting while leaving it available for analysis.
Use [Biome's configuration reference](https://biomejs.dev/reference/configuration/#filesincludes)
for matching and scanner behavior.

In `rome.json`, add the generated directory to `files.ignore` instead:

```json
{
  "files": {
    "ignore": ["**/_generated/**"]
  }
}
```

Merge this into existing settings rather than replacing them. See
[Rome's configuration schema](https://github.com/rome/tools/blob/main/npm/rome/configuration_schema.json).
These examples target `_generated` directories; use a narrower project-relative pattern if needed.
`hulla-db setup --ignore-file` does not edit either tool's JSON configuration.

## Function discovery and operation paths

Discovery follows runtime exports. Type-only reexports, including `export type *`, never register
queries or mutations. Reexporting an operation from a barrel preserves its generated callable;
when its original declaration is also discovered, that declaration supplies the operation ID.
The manifest contains one entry for the operation, shared by its import aliases.

For low-level `discoverFunctions` and `generateFunctions`, `functions` chooses the directory to
scan and `moduleRoot` supplies the independent base for operation paths. `moduleRoot` defaults
to the schema's directory. Set it to a common project directory for declarations outside that
directory, and keep it fixed when widening or narrowing discovery.

Legacy `db.config.ts` projects also accept `moduleRoot`, resolved relative to the config file.
Their default remains the configured `functions` directory, or the schema directory when absent.
Set `moduleRoot` explicitly before changing discovery scope to preserve operation IDs.

```ts
import { defineConfig } from "@hulla/db/config";

export default defineConfig({
  database: { server: { url: "postgresql://localhost/app" } },
  schema: "src/db/schema.ts",
  functions: "src/features",
  moduleRoot: "src",
});
```

Here `src/features/tasks.ts` produces `features/tasks.<export>` identities even if `functions`
later changes to `src`. Moving or renaming the declaration itself still changes its identity.

## Generated-file ownership

Generated TypeScript files separate the ownership comment from imports with a blank line. Import
sorting can then preserve the file-level comment. The generator still rejects unrelated authored
files and symlinks rather than overwriting them. Generated code remains reproducible output; edit
the schema/functions/configuration instead of the generated files.

References: [Prettier ignore files](https://prettier.io/docs/ignore),
[Oxfmt ignore files](https://oxc.rs/docs/guide/usage/formatter/ignore-files),
[Vite+ format configuration](https://viteplus.dev/config/fmt).
