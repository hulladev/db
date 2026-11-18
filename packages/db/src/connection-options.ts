import type { SchemaDefinition } from "./schema.ts";
import type { DatabaseIdentity } from "./database-metadata.ts";
import type { ReplicaSourceOf } from "./table-binding.ts";
import type { HttpRuntimeOptions } from "./http.ts";

export type SourcesOf<S extends SchemaDefinition> = ReplicaSourceOf<S["tables"][keyof S["tables"]]>;
export type SourceConnection<Source extends DatabaseIdentity = DatabaseIdentity> =
  Omit<HttpRuntimeOptions, "baseUrl" | "contracts" | "storage"> & { readonly database: Source; readonly url: string };
type IdentityOf<D> = D extends DatabaseIdentity<infer S> ? DatabaseIdentity<S> : never;
type Matches<Need, Have> = Have extends unknown ? [Need] extends [Have] ? [Have] extends [Need] ? true : never : never : never;
type Uncovered<Need, Have> = Need extends unknown ? [Matches<Need, Have>] extends [never] ? Need : never : never;
export type SourceSettings<S extends SchemaDefinition, C extends readonly SourceConnection<SourcesOf<S>>[]> =
  [SourcesOf<S>] extends [never] ? { readonly sources?: never }
  : { readonly sources: C }
    & ([Uncovered<SourcesOf<S>, IdentityOf<NoInfer<C>[number]["database"]>>] extends [never] ? unknown : { readonly missingSources: Uncovered<SourcesOf<S>, IdentityOf<NoInfer<C>[number]["database"]>> })
    & ([Uncovered<IdentityOf<NoInfer<C>[number]["database"]>, SourcesOf<S>>] extends [never] ? unknown : { readonly unexpectedSources: Uncovered<IdentityOf<NoInfer<C>[number]["database"]>, SourcesOf<S>> });
