import { expect, test } from "bun:test";
import { spawn } from "node:child_process";
import { chmod, mkdtemp, rm, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { tmpdir } from "node:os";
import { createDatabase } from "../src/server-database.ts";
import { defineSchema, defineTable, v } from "../src/schema.ts";

test("broken commit acknowledgements preserve uncertainty and never replay callbacks", async () => {
  const directory = await mkdtemp(resolve(tmpdir(), "hulla-commit-protocol-"));
  const engine = resolve(directory, "engine");
  const previous = process.env.HULLADB_ENGINE;
  try {
    for (const output of ["not json", JSON.stringify({ type: "unexpected" }), "x".repeat(16 * 1024 * 1024 + 65537)]) {
      await writeFile(engine, `#!${process.execPath}\nimport { createInterface } from 'node:readline';\nfor await (const line of createInterface({ input: process.stdin })) { const frame=JSON.parse(line); if(frame.type==='init') console.log('{"type":"ready"}'); else if(frame.type==='begin') console.log('{"type":"begun"}'); else if(frame.type==='commit') process.stdout.write(${JSON.stringify(output)}+'\\n'); }\n`);
      await chmod(engine, 0o700);
      process.env.HULLADB_ENGINE = engine;
      const db = createDatabase(defineSchema({ tasks: defineTable({ title: v.string() }) }), { url: "postgresql://unused/unused" }, { executionMs: 30000 });
      let attempts = 0;
      try {
        await expect(db.transaction(() => { attempts++; return "finished"; }, { retries: 3 })).rejects.toMatchObject({ code: "commit_outcome_unknown" });
        expect(attempts).toBe(1);
      } finally { await db.close(); }
    }
  } finally {
    if (previous === undefined) delete process.env.HULLADB_ENGINE; else process.env.HULLADB_ENGINE = previous;
    await rm(directory, { recursive: true, force: true });
  }
});

test("idle drain retires unused engines while a concurrent callback keeps its lease", async () => {
  const directory = await mkdtemp(resolve(tmpdir(), "hulla-idle-pool-"));
  const engine = resolve(directory, "engine"), log = resolve(directory, "exits");
  const previous = process.env.HULLADB_ENGINE;
  let release!: () => void;
  const barrier = new Promise<void>(resolve => { release = resolve; });
  try {
    await writeFile(engine, `#!${process.execPath}\nimport { createInterface } from 'node:readline';\nimport { appendFileSync } from 'node:fs';\nprocess.on('SIGTERM',()=>{appendFileSync(${JSON.stringify(log)},'closed\\n');process.exit(0)});\nfor await (const line of createInterface({input:process.stdin})) {const frame=JSON.parse(line);const type=({init:'ready',begin:'begun',commit:'committed',rollback:'rolled_back',ping:'pong'})[frame.type]; console.log(JSON.stringify({type}));}\n`);
    await chmod(engine, 0o700);
    process.env.HULLADB_ENGINE = engine;
    const db = createDatabase(defineSchema({ tasks: defineTable({ title: v.string() }) }), { url: "postgresql://unused/unused" }, { executionMs: 30000 });
    let entered!: () => void;
    const inside = new Promise<void>(resolve => { entered = resolve; });
    const active = db.transaction(async () => { entered(); await barrier; return "active completed"; });
    await inside;
    await db.transaction(() => "other completed");
    await new Promise(resolve => setTimeout(resolve, 10_500));
    const { readFile } = await import("node:fs/promises");
    expect(await readFile(log, "utf8")).toBe("closed\n");
    release();
    expect(await active).toBe("active completed");
    // Reuse validates the surviving lease and close clears its retirement timer.
    expect(await db.transaction(() => "reused")).toBe("reused");
    await db.close();
    await expect(db.transaction(() => "closed")).rejects.toMatchObject({ code: "database_closed" });
  } finally {
    release();
    if (previous === undefined) delete process.env.HULLADB_ENGINE; else process.env.HULLADB_ENGINE = previous;
    await rm(directory, { recursive: true, force: true });
  }
}, 15_000);

test("a disconnected idle connection is replaced before entering another callback", async () => {
  const directory = await mkdtemp(resolve(tmpdir(), "hulla-stale-pool-"));
  const engine = resolve(directory, "engine");
  const previous = process.env.HULLADB_ENGINE;
  try {
    await writeFile(engine, `#!${process.execPath}\nimport { createInterface } from 'node:readline';\nfor await (const line of createInterface({input:process.stdin})) {const frame=JSON.parse(line);if(frame.type==='ping'){process.exit(0)}const type=({init:'ready',begin:'begun',commit:'committed',rollback:'rolled_back'})[frame.type];console.log(JSON.stringify({type}));}\n`);
    await chmod(engine, 0o700);
    process.env.HULLADB_ENGINE = engine;
    const db = createDatabase(defineSchema({ tasks: defineTable({ title: v.string() }) }), { url: "postgresql://unused/unused" }, { executionMs: 30000 });
    try {
      await db.transaction(() => "first");
      let attempts = 0;
      expect(await db.transaction(() => { attempts++; return "replacement"; })).toBe("replacement");
      expect(attempts).toBe(1);
    } finally { await db.close(); }
  } finally {
    if (previous === undefined) delete process.env.HULLADB_ENGINE; else process.env.HULLADB_ENGINE = previous;
    await rm(directory, { recursive: true, force: true });
  }
});

test("a Node host exits after its idle engine drains without explicit close", async () => {
  const directory = await mkdtemp(resolve(tmpdir(), "hulla-host-exit-"));
  const engine = resolve(directory, "engine"), host = resolve(directory, "host.mjs");
  try {
    await writeFile(engine, `#!${process.execPath}\nimport { createInterface } from 'node:readline';\nfor await (const line of createInterface({input:process.stdin})) {const frame=JSON.parse(line); console.log(JSON.stringify({type:({init:'ready',begin:'begun',commit:'committed'})[frame.type]}));}\n`);
    await chmod(engine, 0o700);
    await writeFile(host, `import {createDatabase} from ${JSON.stringify(new URL("../src/server-database.ts", import.meta.url).href)};
import {defineSchema,defineTable,v} from ${JSON.stringify(new URL("../src/schema.ts", import.meta.url).href)};
const db=createDatabase(defineSchema({tasks:defineTable({title:v.string()})}),{url:'postgresql://unused/unused'});
await db.transaction(()=> 'finished');
`);
    const child = spawn(process.env.NODE ?? "node", [host], { env: { ...process.env, HULLADB_ENGINE: engine }, stdio: ["ignore", "ignore", "pipe"] });
    let stderr = "";
    child.stderr.on("data", chunk => { stderr += chunk; });
    const timer = setTimeout(() => child.kill("SIGKILL"), 14_000);
    try {
      const result = await new Promise<{code: number | null; signal: string | null}>((resolve, reject) => {
        child.on("error", reject);
        child.on("exit", (code, signal) => resolve({code, signal}));
      });
      expect(stderr).toBe("");
      expect(result).toEqual({ code: 0, signal: null });
    } finally { clearTimeout(timer); child.kill(); }
  } finally { await rm(directory, { recursive: true, force: true }); }
}, 16_000);


test("Node and Bun bound bridge admission and release blocked callbacks on close and deadline", async () => {
  const directory = await mkdtemp(resolve(tmpdir(), "hulla-bridge-limits-"));
  const engine = resolve(directory, "engine"), host = resolve(directory, "host.mjs");
  try {
    await writeFile(engine, `#!${process.execPath}\nimport { createInterface } from 'node:readline';
process.on('SIGTERM',()=>{});
for await (const line of createInterface({input:process.stdin})) {const frame=JSON.parse(line); console.log(JSON.stringify({type:({init:'ready',begin:'begun',commit:'committed',rollback:'rolled_back',ping:'pong'})[frame.type]}));}
`);
    await chmod(engine, 0o700);
    await writeFile(host, `import assert from 'node:assert/strict';
import {createDatabase} from ${JSON.stringify(new URL("../src/server-database.ts", import.meta.url).href)};
import {defineSchema,defineTable,v} from ${JSON.stringify(new URL("../src/schema.ts", import.meta.url).href)};
const schema=defineSchema({tasks:defineTable({title:v.string()})});
const open=()=>createDatabase(schema,{url:'postgresql://unused/unused'},{executionMs:30000});
const db=open(); let entered=0,queued=0;
const active=Array.from({length:4},()=>db.transaction(()=>{entered++;return new Promise(()=>{});}).catch(error=>error));
while(entered!==4) await new Promise(resolve=>setTimeout(resolve,1));
const waiting=Array.from({length:64},()=>db.transaction(()=>{queued++;}).catch(error=>error));
await assert.rejects(db.transaction(()=>{}),{code:'capacity'});
await db.close();
for(const result of await Promise.all([...active,...waiting])) assert.equal(result.code,'database_closed');
assert.equal(queued,0);
// Drive the real deadline callback after the handler has stopped making SQL calls.
const timers=new Map(),schedule=globalThis.setTimeout,clear=globalThis.clearTimeout;
globalThis.setTimeout=(fn,ms,...args)=>{const id=schedule(fn,ms,...args);if(ms===30000)timers.set(id,()=>fn(...args));return id;};
globalThis.clearTimeout=id=>{timers.delete(id);clear(id);};
const next=open(); let inside=false,late;
try {
  const stalled=next.transaction(tx=>{inside=true;late=tx;return new Promise(()=>{});}).catch(error=>error);
  while(!inside) await new Promise(resolve=>schedule(resolve,1));
  for(const fire of [...timers.values()]) fire();
  assert.equal((await stalled).code,'transaction_timeout');
  await assert.rejects(Promise.resolve(late.select('tasks')),{code:'transaction_closed'});
  assert.equal(await next.transaction(()=> 'recovered'),'recovered');
} finally {globalThis.setTimeout=schedule;globalThis.clearTimeout=clear;await next.close();}
`);
    for (const executable of [process.env.NODE ?? "node", process.execPath]) {
      const child = spawn(executable, [host], { env: { ...process.env, HULLADB_ENGINE: engine }, stdio: ["ignore", "ignore", "pipe"] });
      let stderr = "";
      child.stderr.on("data", chunk => { stderr += chunk; });
      const timer = setTimeout(() => child.kill("SIGKILL"), 10_000);
      try {
        const code = await new Promise<number | null>((resolve, reject) => { child.on("error", reject); child.on("exit", resolve); });
        expect(stderr).toBe(""); expect(code).toBe(0);
      } finally { clearTimeout(timer); child.kill(); }
    }
  } finally { await rm(directory, { recursive: true, force: true }); }
}, 22_000);
