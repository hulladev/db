import { query } from "./server.ts";

// Names valid in application code must not collide with codegen's own imports.
export const runtime = query(({ identity }) => identity);
export const queryBinding = query(({ identity }) => identity);
export const $definitions = query(({ identity }) => identity);
