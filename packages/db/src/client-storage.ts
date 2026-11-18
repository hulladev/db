import type { Session, Snapshot } from "./http/protocol.ts";
import type { DatabaseIdentity } from "./database-metadata.ts";
import type { FunctionCodecs } from "./codec.ts";

export interface StoredSession extends Session { readonly source: string }
export interface ReplicaContract { readonly table: string; readonly columns: readonly string[] }
export interface FunctionContract extends FunctionCodecs { readonly kind: "query" | "mutation"; readonly replica?: ReplicaContract }
export type FunctionContracts = Readonly<Record<string, FunctionContract>>;
export type ReplicaChange = { readonly query: string } & ({ readonly snapshot: Snapshot; readonly replica: ReplicaContract; readonly rows: unknown } | { readonly snapshot?: never });
export interface QueuedMutation {
  readonly id: string;
  readonly name: string;
  readonly input: unknown;
  readonly owner: StoredSession;
  readonly attempt: number;
  readonly state: "queued" | "sent" | "conflict" | "failed" | "committed";
  readonly requestId: string;
  readonly ticket?: string;
  readonly result?: unknown;
  readonly error?: unknown;
}

/** Engine capability. Only verified server envelopes enter persistent replica storage. */
export interface ClientStorage {
  activate(key: string | null): Promise<void>;
  deactivate(): void;
  session(key: string): Promise<StoredSession | undefined>;
  bind(key: string, session: StoredSession): Promise<void>;
  mutations(key: string): Promise<readonly QueuedMutation[]>;
  enqueue(key: string, mutation: QueuedMutation): Promise<void>;
  update(key: string, mutation: QueuedMutation, previous: QueuedMutation): Promise<boolean>;
  remove(key: string, id: string): Promise<void>;
  snapshot(key: string, query: string): Promise<Snapshot | undefined>;
  apply(key: string, changes: readonly ReplicaChange[]): Promise<void>;
  saveSnapshot(key: string, query: string, snapshot: Snapshot, replica: ReplicaContract, rows: unknown): Promise<void>;
  forgetSnapshot(key: string, query: string): Promise<void>;
}

type Factory = (source: DatabaseIdentity) => ClientStorage;
const factories = new WeakMap<object, Factory>();
export function registerClientStorage(database: object, factory: Factory): void { factories.set(database, factory); }
export function inheritClientStorage(from: object, to: object): void {
  const factory = factories.get(from);
  if (factory) factories.set(to, factory);
}
export function clientStorage(database: object, source: DatabaseIdentity): ClientStorage {
  const factory = factories.get(database);
  if (!factory) throw new TypeError("This engine does not support durable device storage");
  return factory(source);
}
