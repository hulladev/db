import { defineConfig } from "@hulla/db/config";

// Uses the local development database, or DATABASE_URL when provided.
export default defineConfig({
  database: { server: { namespace: "studio_example" } },
  schema: "schema.ts",
  service: { port: 3001 },
});
