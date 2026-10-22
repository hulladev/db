import { DatabaseSync } from "node:sqlite";
import type { FunctionRuntime } from "../src/contracts.ts";
import { executeDefinition, isFunctionDefinition } from "../src/declarations.ts";
import * as notes from "./fixtures/notes.ts";
import type { Note, ReadContext, WriteContext } from "./fixtures/server.ts";

/** Test storage for declaration execution. Real transaction ownership and recovery
 * are covered by the worker and PostgreSQL integration tests. */
export function storageFixture(modules: Readonly<Record<string, Readonly<Record<string, unknown>>>> = {}) {
  const db = new DatabaseSync(":memory:");
  db.exec(
    "CREATE TABLE notes (id INTEGER PRIMARY KEY, title TEXT NOT NULL, owner TEXT NOT NULL, UNIQUE(owner, title))",
  );
  const select = async (owner: string) =>
    db
      .prepare("SELECT id, title, owner FROM notes WHERE owner = ? ORDER BY id")
      .all(owner) as unknown as Note[];
  const insert = async (title: string, owner: string) =>
    db
      .prepare("INSERT INTO notes(title, owner) VALUES (?, ?) RETURNING id, title, owner")
      .get(title, owner) as unknown as Note;

  const definitions = new Map<string, Parameters<typeof executeDefinition>[0]>();
  for (const [module, exports] of Object.entries({ notes, ...modules })) {
    for (const [name, definition] of Object.entries(exports)) {
      if (isFunctionDefinition(definition)) definitions.set(`${module}.${name}`, definition);
    }
  }

  function runtime(identity: string | null): FunctionRuntime {
    const read: ReadContext = Object.freeze({ identity, db: Object.freeze({ select }) });
    const write: WriteContext = Object.freeze({ identity, db: Object.freeze({ select, insert }) });
    return {
      async call(operation, options) {
        options.signal?.throwIfAborted();
        const definition = definitions.get(operation.name);
        if (!definition) throw new Error(`Unexpected fixture function: ${operation.name}`);
        if (definition.kind === "query") return executeDefinition(definition, read, operation.input);
        db.exec("BEGIN");
        try {
          const result = await executeDefinition(definition, write, operation.input);
          options.signal?.throwIfAborted();
          db.exec("COMMIT");
          return result;
        } catch (error) {
          db.exec("ROLLBACK");
          throw error;
        }
      },
    };
  }
  return { runtime, close: () => db.close() };
}
