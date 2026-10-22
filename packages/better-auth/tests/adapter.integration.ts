import { strict as assert } from "node:assert";
import { betterAuth, type BetterAuthOptions } from "better-auth";
import { parseSetCookieHeader } from "better-auth/cookies";
import { createDatabase } from "@hulla/db/server";
import { defineSchema } from "@hulla/db/schema";
import { initializeDatabase } from "../../db/src/server-database.ts";
import { hullaAdapter } from "../src/index.ts";
import { betterAuthTables } from "../src/schema.ts";

const url = process.env.HULLADB_TEST_URL;
const namespace = process.env.HULLADB_TEST_NAMESPACE;
const engine = process.env.HULLADB_ENGINE ?? process.env.HULLADB_TEST_ENGINE;
if (!url || !namespace || !engine) throw new Error("Better Auth integration requires HULLADB_TEST_URL, HULLADB_TEST_NAMESPACE and HULLADB_ENGINE");
process.env.HULLADB_ENGINE = engine;

const baseURL = "http://localhost:3107";
const settings = {
  baseURL,
  secret: "hulla-better-auth-integration-fixture-secret-2026",
  emailAndPassword: { enabled: true },
  session: {
    modelName: "AuthSessions",
    fields: { token: "session_token", userId: "member_id", expiresAt: "expires_at" },
    cookieCache: { enabled: false },
  },
  user: {
    modelName: "AuthUsers",
    fields: { email: "email_address", name: "display_name", emailVerified: "email_verified" },
    additionalFields: {
      tenant: { type: "string", required: true, fieldName: "tenant_key" },
      credits: { type: "number", required: true, defaultValue: 0, fieldName: "remaining_credits" },
    },
  },
  account: { modelName: "AuthAccounts", fields: { userId: "member_id", password: "password_hash" } },
  verification: { modelName: "AuthVerifications", fields: { identifier: "lookup_key", value: "secret_value" } },
} satisfies BetterAuthOptions;
const schema = defineSchema(betterAuthTables(settings));
const connection = { url, namespace };
await initializeDatabase(schema, connection);
let db = createDatabase(schema, connection);

interface AuthUser {
  id: string;
  name: string;
  email: string;
  emailVerified: boolean;
  image: string | null;
  createdAt: Date;
  updatedAt: Date;
  tenant: string;
  credits: number;
}
interface Verification { id: string; identifier: string; value: string; expiresAt: Date; createdAt: Date; updatedAt: Date }

function requestHeaders(response: Response): Headers {
  const cookies = parseSetCookieHeader(response.headers.get("set-cookie") ?? "");
  assert.ok(cookies.size, "authentication must issue a cookie");
  return new Headers({ origin: baseURL, cookie: [...cookies].map(([name, cookie]) => `${name}=${cookie.value}`).join("; ") });
}

try {
  let auth = betterAuth({ ...settings, database: hullaAdapter(db) });
  const email = "member@example.test";
  const password = "correct-integration-password-2026";
  const signedUp = await auth.api.signUpEmail({
    body: { name: "Persisted Member", email, password, tenant: "application" },
    headers: new Headers({ origin: baseURL }),
    asResponse: true,
  });
  assert.equal(signedUp.status, 200, await signedUp.clone().text());
  const signup = await signedUp.json() as { user: { id: string; email: string }; token: string };
  assert.equal(signup.user.email, email);
  assert.ok(signup.user.id);
  const signupHeaders = requestHeaders(signedUp);
  const stored = await db.select("AuthUsers").where("email_address", "=", email).unique();
  assert.equal(stored?.id, signup.user.id);
  assert.equal(stored?.display_name, "Persisted Member");
  assert.equal(stored?.tenant_key, "application");
  assert.equal(stored?.remaining_credits, 0);
  assert.ok(stored?.createdAt instanceof Date);
  const account = await db.select("AuthAccounts").where("member_id", "=", signup.user.id).unique();
  assert.equal(account?.providerId, "credential");
  assert.equal(typeof account?.password_hash, "string");
  assert.notEqual(account?.password_hash, password);

  // Reopen the native database and auth instance: session recovery must depend
  // on durable rows rather than an auth or database object's in-memory state.
  await db.close();
  db = createDatabase(schema, connection);
  auth = betterAuth({ ...settings, database: hullaAdapter(db) });
  const restored = await auth.api.getSession({ headers: signupHeaders });
  assert.equal(restored?.user.id, signup.user.id);
  assert.equal(restored?.user.email, email);
  assert.ok(restored?.session.expiresAt instanceof Date);
  const signedOut = await auth.api.signOut({ headers: signupHeaders });
  assert.equal(signedOut.success, true);
  assert.equal(await auth.api.getSession({ headers: signupHeaders }), null);
  assert.equal((await db.select("AuthSessions").where("member_id", "=", signup.user.id)).length, 0);

  await assert.rejects(auth.api.signInEmail({ body: { email, password: "wrong-password-2026" }, headers: new Headers({ origin: baseURL }) }));
  const signedIn = await auth.api.signInEmail({ body: { email, password }, headers: new Headers({ origin: baseURL }), asResponse: true });
  assert.equal(signedIn.status, 200, await signedIn.clone().text());
  const signinHeaders = requestHeaders(signedIn);
  assert.equal((await auth.api.getSession({ headers: signinHeaders }))?.user.id, signup.user.id);

  const adapter = hullaAdapter(db)(settings);
  const createdAt = new Date("2026-01-02T03:04:05.006Z");
  const fixtureUser = async (id: string, name: string, credits: number) => {
    const data = { id, name, email: `${id}@example.test`, emailVerified: false, image: null, createdAt, updatedAt: createdAt, tenant: "adapter-fixture", credits };
    return adapter.create<AuthUser>({ model: "user", data, forceAllowId: true });
  };
  const alpha = await fixtureUser("adapter-alpha", "Alpha", 1);
  await fixtureUser("adapter-beta", "Beta", 2);
  await fixtureUser("adapter-gamma", "Gamma", 3);
  assert.equal(alpha.id, "adapter-alpha");
  assert.equal(alpha.name, "Alpha");
  assert.equal(alpha.tenant, "adapter-fixture");
  assert.equal(alpha.createdAt.getTime(), createdAt.getTime());
  assert.equal(await adapter.count({ model: "user", where: [{ field: "tenant", value: "adapter-fixture" }] }), 3);
  const page = await adapter.findMany<AuthUser>({
    model: "user", where: [{ field: "tenant", value: "adapter-fixture" }], sortBy: { field: "name", direction: "desc" }, offset: 1, limit: 1,
  });
  assert.deepEqual(page.map(user => user.name), ["Beta"]);
  const projection = await adapter.findOne<{ email: string; name: string }>({ model: "user", where: [{ field: "email", value: "ADAPTER-ALPHA@EXAMPLE.TEST", mode: "insensitive" }], select: ["email", "name"] });
  assert.equal(projection?.email, alpha.email);
  assert.equal(projection?.name, "Alpha");
  assert.equal(Object.hasOwn(projection!, "email_address"), false);
  // Better Auth uses an empty selection to mean every field, rather than an
  // empty SQL projection. Preserve that contract at the adapter boundary.
  const allFields = await adapter.findOne<AuthUser>({ model: "user", where: [{ field: "id", value: alpha.id }], select: [] });
  assert.equal(allFields?.name, "Alpha");
  assert.equal(allFields?.email, alpha.email);
  assert.equal((await adapter.findMany<AuthUser>({ model: "user", where: [{ field: "id", value: alpha.id }], select: [] }))[0]?.name, "Alpha");
  const selected = await adapter.findMany<AuthUser>({ model: "user", where: [
    { field: "tenant", value: "adapter-fixture" },
    { field: "credits", operator: "gte", value: 2 },
    { field: "name", operator: "in", value: ["Beta", "Gamma"] },
  ], sortBy: { field: "credits", direction: "asc" } });
  assert.deepEqual(selected.map(user => user.name), ["Beta", "Gamma"]);
  const alternatives = await adapter.findMany<AuthUser>({ model: "user", where: [
    { field: "tenant", value: "adapter-fixture" },
    { field: "name", value: "Alpha", connector: "OR" },
    { field: "name", value: "Gamma", connector: "OR" },
  ], sortBy: { field: "name", direction: "asc" } });
  assert.deepEqual(alternatives.map(user => user.name), ["Alpha", "Gamma"]);
  assert.equal((await adapter.update<AuthUser>({ model: "user", where: [{ field: "id", value: alpha.id }], update: { name: "Updated Alpha" } }))?.name, "Updated Alpha");
  assert.equal((await db.select("AuthUsers").where("id", "=", alpha.id).unique())?.display_name, "Updated Alpha");
  assert.equal(await adapter.updateMany({ model: "user", where: [{ field: "tenant", value: "adapter-fixture" }, { field: "credits", operator: "gt", value: 1 }], update: { emailVerified: true } }), 2);
  assert.equal((await adapter.findOne<AuthUser>({ model: "user", where: [{ field: "id", value: alpha.id }] }))?.emailVerified, false);

  await assert.rejects(adapter.transaction(async tx => {
    await tx.update({ model: "user", where: [{ field: "id", value: alpha.id }], update: { name: "Must Roll Back" } });
    await tx.create({ model: "verification", data: { identifier: "rollback", value: "rollback-secret", expiresAt: new Date(Date.now() + 60_000), createdAt, updatedAt: createdAt } });
    throw new Error("adapter rollback sentinel");
  }), /adapter rollback sentinel/);
  assert.equal((await adapter.findOne<AuthUser>({ model: "user", where: [{ field: "id", value: alpha.id }] }))?.name, "Updated Alpha");
  assert.equal(await adapter.count({ model: "verification", where: [{ field: "identifier", value: "rollback" }] }), 0);
  await adapter.transaction(async tx => {
    await tx.update({ model: "user", where: [{ field: "id", value: alpha.id }], update: { credits: 7 } });
    assert.equal((await tx.findOne<AuthUser>({ model: "user", where: [{ field: "id", value: alpha.id }] }))?.credits, 7);
  });
  assert.equal((await db.select("AuthUsers").where("id", "=", alpha.id).unique())?.remaining_credits, 7);

  await adapter.update({ model: "user", where: [{ field: "id", value: alpha.id }], update: { credits: 1 } });
  const decremented = await Promise.all([0, 1].map(() => adapter.incrementOne<AuthUser>({ model: "user", where: [
    { field: "id", value: alpha.id }, { field: "credits", operator: "gt", value: 0 },
  ], increment: { credits: -1 } })));
  assert.equal(decremented.filter(Boolean).length, 1, "a guarded counter must not be spent twice");
  assert.equal((await adapter.findOne<AuthUser>({ model: "user", where: [{ field: "id", value: alpha.id }] }))?.credits, 0);

  const verification = await adapter.create<Verification>({ model: "verification", data: { identifier: "single-use", value: "one-time-secret", expiresAt: new Date(Date.now() + 60_000), createdAt, updatedAt: createdAt } });
  const consumed = await Promise.all([0, 1].map(() => adapter.consumeOne<Verification>({ model: "verification", where: [{ field: "id", value: verification.id }] })));
  assert.equal(consumed.filter(Boolean).length, 1, "only one contender may receive a single-use credential");
  assert.equal(consumed.find(Boolean)?.value, "one-time-secret");
  assert.equal(await adapter.findOne({ model: "verification", where: [{ field: "id", value: verification.id }] }), null);
  for (const value of ["first", "second"]) await adapter.create({ model: "verification", data: { identifier: "shared-identifier", value, expiresAt: new Date(Date.now() + 60_000), createdAt, updatedAt: createdAt } });
  assert.ok(await adapter.consumeOne({ model: "verification", where: [{ field: "identifier", value: "shared-identifier" }] }));
  assert.equal(await adapter.count({ model: "verification", where: [{ field: "identifier", value: "shared-identifier" }] }), 1, "a nonunique match may consume only one record");
  await adapter.deleteMany({ model: "verification", where: [{ field: "identifier", value: "shared-identifier" }] });

  // A direct parent deletion must trigger PostgreSQL FK cascades independently
  // of Better Auth's own explicit session/account cleanup paths.
  assert.equal(await adapter.count({ model: "session", where: [{ field: "userId", value: signup.user.id }] }), 1);
  await adapter.delete({ model: "user", where: [{ field: "id", value: signup.user.id }] });
  assert.equal((await db.select("AuthAccounts").where("member_id", "=", signup.user.id)).length, 0);
  assert.equal((await db.select("AuthSessions").where("member_id", "=", signup.user.id)).length, 0);
  assert.equal(await auth.api.getSession({ headers: signinHeaders }), null);
  await adapter.delete({ model: "user", where: [{ field: "id", value: alpha.id }] });
  assert.equal(await adapter.deleteMany({ model: "user", where: [{ field: "tenant", value: "adapter-fixture" }] }), 2);
  assert.equal(await adapter.count({ model: "user", where: [{ field: "tenant", value: "adapter-fixture" }] }), 0);
  console.log("Better Auth PostgreSQL adapter integration passed");
} finally {
  await db.close();
}
