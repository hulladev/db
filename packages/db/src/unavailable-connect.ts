import type { DatabaseConfiguration } from "./database-configuration.ts";
import type { SchemaDefinition } from "./schema.ts";
export function sourceRuntime(..._args: unknown[]): never { throw new Error("Source connections are unavailable in a managed worker"); }
import type { ConnectedDefinition } from "./engine.ts";

export async function openDatabase<S extends SchemaDefinition>(
  _definition: DatabaseConfiguration<S>, _connection: unknown, _initialize = false,
): Promise<ConnectedDefinition<S>> {
  throw new Error("Database declarations are inert in restricted workers; use ctx.db inside handlers");
}
