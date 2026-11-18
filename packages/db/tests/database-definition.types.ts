import { defineSchema, defineTable, v } from '../src/index.ts';
import { sqlite } from '@hulla/db-sqlite';
import { postgres } from '@hulla/db-postgres';
import { schemaIdentity } from '../src/database-metadata.ts';
import type { Input, Output, MutationCallable } from '../src/index.ts';
import { objectInput } from './fixtures/standard.ts';

// Compile-only coverage for authoring ownership, source completeness, and inferred callables.
async function contracts() {
  const drafts = defineTable({ title: v.string(), body: v.string(), done: v.boolean().default(false) });
  const publications = drafts.extend({ body: v.string().nullable(), publishedAt: v.timestamp() });
  const summaries = publications.pick('title', 'publishedAt');
  const summary: typeof summaries.$inferInsert = { title: 'Summary', publishedAt: new Date() };
  // @ts-expect-error omitted fields remain absent
  summary.body;
  // @ts-expect-error composition does not change its source
  drafts.fields.publishedAt;
  // @ts-expect-error overridden validators replace their old representations
  const invalid: typeof publications.$inferInsert = { title: 'Invalid', body: 1, publishedAt: new Date() };
  // @ts-expect-error unknown projection fields
  drafts.pick('missing');
  const schema = defineSchema({ publications });
  const primary = schemaIdentity(schema);
  sqlite({ schema, filename: 'app.sqlite', functions: './functions/**/*.cjs' });
  // @ts-expect-error registration accepts one barrel loader or one glob
  sqlite({ schema, filename: 'app.sqlite', functions: [] });
  // @ts-expect-error registry objects are not selectors
  sqlite({ schema, filename: 'app.sqlite', functions: {} });
  // @ts-expect-error an import loader must return a promise
  sqlite({ schema, filename: 'app.sqlite', functions: () => 123 });
  // @ts-expect-error URL belongs on sources, not an endpoint declaration
  sqlite({ schema, filename: 'app.sqlite', endpoint: 'https://example.com' });
  // @ts-expect-error SQLite needs one platform storage configuration
  sqlite({ schema });
  postgres({ schema });
  postgres({ schema, url: process.env.DATABASE_URL });
  // @ts-expect-error PostgreSQL cannot take SQLite file settings
  postgres({ schema, filename: 'app.sqlite' });
  const publicPublications = schema.tables.publications.omit('body');
  // @ts-expect-error preserve primary keys
  schema.tables.publications.omit('id');
  // @ts-expect-error projections require the primary key
  schema.tables.publications.pick('title');
  // @ts-expect-error bound tables cannot invent source columns
  schema.tables.publications.extend({ extra: v.string() });
  const device = defineSchema({ publications: publicPublications.replica() });
  // @ts-expect-error replica declarations cannot become writable through composition
  device.tables.publications.extend({ field: v.string() });
  const client = sqlite({ schema: device, filename: ':memory:', sources: [{ database: primary, url: 'https://api.example.com' }] });
  // @ts-expect-error required sources cannot be omitted
  sqlite({ schema: device, filename: 'local.sqlite' });
  // @ts-expect-error an empty array does not cover a declared source
  sqlite({ schema: device, filename: 'local.sqlite', sources: [] });
  const catalog = schemaIdentity(defineSchema({ products: defineTable({ name: v.string() }) }));
  const combined = defineSchema({ publications: publicPublications.replica(), products: catalog.tables.products.replica() });
  sqlite({ schema: combined, filename: 'all.sqlite', sources: [{ database: primary, url: 'https://app.example.com' }, { database: catalog, url: 'https://catalog.example.com' }] });
  // @ts-expect-error every distinct source must be supplied
  sqlite({ schema: combined, filename: 'all.sqlite', sources: [{ database: primary, url: 'https://app.example.com' }] });
  // @ts-expect-error sources must own tables in this schema
  sqlite({ schema: device, filename: 'local.sqlite', sources: [{ database: catalog, url: 'https://catalog.example.com' }] });
  // @ts-expect-error replica storage is readonly
  client.insert('publications', { title: 'Unsynced', publishedAt: new Date() });
  // @ts-expect-error generated operation inputs and owner must be present
  client.resolveConflict({ id: 'id' }, { action: 'reject' });
  const authenticated = schema.query.extend(({ identity }) => {
    if (typeof identity !== 'string') throw new Error('Sign in required');
    return { workspace: identity };
  });
  const selected = authenticated({ replica: publicPublications, input: objectInput({ title: v.string() }), handler: ({ db }, input) => db.select(publicPublications).where({ title: input.title }) });
  const input: Input<typeof selected> = { title: 'Query' };
  const output: Output<typeof selected> = [];
  // @ts-expect-error omitted columns do not become optional output fields
  output[0]!.body;
  // @ts-expect-error query contexts expose readonly database access
  schema.query(({ db }) => db.insert('publications', {}));
  const { query } = schema;
  const q = query(({ db }) => db.select('publications'));
  const local = sqlite({ schema, filename: 'local.sqlite', functions: async () => ({ queries: { q } }) });
  const results = await local.functions.queries.q();
  const direct = local.bind({ queries: { q } });
  const directRows = await direct.queries.q();
  const directTitle: string | undefined = directRows[0]?.title;
  const title: string | undefined = results[0]?.title;
  // @ts-expect-error the function catalogue preserves namespace and export names
  local.functions.missing();
  // @ts-expect-error local calls retain their input arity
  local.functions.queries.q({ title: 'extra' });
  // @ts-expect-error direct declaration bindings preserve input arity
  direct.queries.q({ title: 'extra' });
  // @ts-expect-error createDatabase/connection helpers do not belong to schema declarations
  schema.connect({});
  declareCallable();
  function declareCallable() {
    const mutation = null as unknown as MutationCallable<{ readonly kind: 'mutation'; readonly $types: { input: { title: string }; output: string } }>;
    mutation.offline(() => ({ action: 'defer' }));
    // @ts-expect-error queuing always needs an explicit resolver
    mutation.offline();
  }
  void [invalid, input, title, directTitle];
}
void contracts;
