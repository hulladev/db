import { postgresFactory } from "./descriptor.ts";
export type { PostgresConnection } from "./descriptor.ts";

/** Engine identity remains usable in browser schemas and restricted declarations. */
export const postgres = postgresFactory(async () => { throw new Error("PostgreSQL connections require a server runtime; use ctx.db inside database functions"); });
