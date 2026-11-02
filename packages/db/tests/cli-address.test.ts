import { expect, test } from "bun:test";
import { createServer, type Server } from "node:net";
import { PassThrough, Writable } from "node:stream";
import { AddressConflict, startCLIListener, StartupCancelled } from "../src/cli-address.ts";

async function listen(host = "127.0.0.1", port = 0): Promise<Server> {
  const server = createServer();
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen({ host, port, exclusive: true }, resolve);
  });
  return server;
}
function port(server: Server): number { return (server.address() as { port: number }).port; }
async function close(server: Server) {
  if (server.listening) await new Promise<void>(resolve => server.close(() => resolve()));
}
async function occupiedPair(): Promise<[Server, Server]> {
  for (let attempt = 0; attempt < 20; attempt++) {
    const first = await listen();
    if (port(first) > 65000) { await close(first); continue; }
    try { return [first, await listen("127.0.0.1", port(first) + 1)]; }
    catch (error) {
      await close(first);
      if ((error as NodeJS.ErrnoException).code !== "EADDRINUSE") throw error;
    }
  }
  throw new Error("Cannot reserve adjacent test ports");
}
function terminal() {
  const input = Object.assign(new PassThrough(), { isTTY: true });
  let text = "";
  const output = Object.assign(new Writable({ write(chunk, _encoding, done) { text += chunk.toString(); done(); } }), { isTTY: true });
  let answered = 0;
  return {
    streams: { input, output },
    async answer(value: string) {
      const target = ++answered, deadline = Date.now() + 2000;
      while ((text.match(/(?:Choose \[1\]|blank to go back\)): /g) ?? []).length < target) {
        if (Date.now() > deadline) throw new Error(`Prompt timed out: ${text}`);
        await Bun.sleep(5);
      }
      input.write(value + "\n");
    },
    text: () => text,
  };
}

test("Enter selects the next free port and skips other occupied addresses", async () => {
  const [occupied, adjacent] = await occupiedPair();
  let started: Server | undefined;
  const tty = terminal();
  try {
    const pending = startCLIListener("Service", { host: "127.0.0.1", port: port(occupied) },
      address => listen(address.host, address.port), { signal: new AbortController().signal, streams: tty.streams });
    await tty.answer("");
    const result = await pending;
    started = result.listener;
    expect(result.address.port).toBeGreaterThan(port(adjacent));
    expect(port(started)).toBe(result.address.port);
    expect(tty.text()).toContain("next available port (default)");
  } finally { await Promise.all([close(occupied), close(adjacent), ...(started ? [close(started)] : [])]); }
});

test("custom addresses validate input and keep Studio on its loopback host", async () => {
  const occupied = await listen(), available = await listen();
  const selected = port(available);
  await close(available);
  const tty = terminal();
  let started: Server | undefined;
  try {
    const pending = startCLIListener("Studio", { host: "127.0.0.1", port: port(occupied) },
      address => listen(address.host, address.port), { signal: new AbortController().signal, streams: tty.streams, fixedHost: true });
    await tty.answer("2");
    await tty.answer("70000");
    await tty.answer(`0.0.0.0:${selected}`);
    await tty.answer(`http://127.0.0.1:${selected}`);
    const result = await pending;
    started = result.listener;
    expect(result.address).toEqual({ host: "127.0.0.1", port: selected });
    expect(tty.text()).toContain("from 1 to 65535");
    expect(tty.text()).toContain("must stay on 127.0.0.1");
  } finally { await close(occupied); if (started) await close(started); }
});

test("exit, EOF, Ctrl-C and shutdown cancel a pending prompt without opening another listener", async () => {
  const occupied = await listen();
  try {
    for (const action of ["exit", "eof", "interrupt", "abort"] as const) {
      const tty = terminal(), controller = new AbortController();
      const pending = startCLIListener("Service", { host: "127.0.0.1", port: port(occupied) },
        address => listen(address.host, address.port), { signal: controller.signal, streams: tty.streams });
      const result = pending.then(() => undefined, error => error);
      if (action === "exit") await tty.answer("3");
      else {
        // Wait for the actual question before ending its input or interrupting it.
        const deadline = Date.now() + 2000;
        while (!tty.text().includes("Choose [1]: ")) {
          if (Date.now() > deadline) throw new Error("Cancellation prompt timed out");
          await Bun.sleep(5);
        }
        if (action === "eof") tty.streams.input.end();
        else if (action === "interrupt") tty.streams.input.write("\x03");
        else controller.abort();
      }
      expect(await result).toBeInstanceOf(StartupCancelled);
    }
  } finally { await close(occupied); }
});

test("Node can recover a real bind conflict using the default prompt selection", () => {
  const node = Bun.which("node");
  expect(node).not.toBeNull();
  const module = new URL("../src/cli-address.ts", import.meta.url).href;
  const child = Bun.spawnSync([node!, "--input-type=module", "-e", `
    import assert from 'node:assert/strict';
    import {createServer} from 'node:net';
    import {PassThrough, Writable} from 'node:stream';
    import {startCLIListener} from ${JSON.stringify(module)};
    async function listen(address) {
      const server=createServer();
      await new Promise((resolve,reject)=>{server.once('error',reject);server.listen(address.port,address.host,resolve);});
      return server;
    }
    const occupied=await listen({host:'127.0.0.1',port:0});
    const input=Object.assign(new PassThrough(),{isTTY:true});
    const output=Object.assign(new Writable({write(chunk,encoding,done){
      if(chunk.toString().includes('Choose [1]: ')) queueMicrotask(()=>input.write('\\n'));
      done();
    }}),{isTTY:true});
    let started;
    try {
      const result=await startCLIListener('Service',{host:'127.0.0.1',port:occupied.address().port},listen,
        {signal:new AbortController().signal,streams:{input,output}});
      started=result.listener;
      assert(result.address.port>occupied.address().port);
      assert.equal(started.address().port,result.address.port);
    } finally {
      await Promise.all([occupied,started].filter(Boolean).map(server=>new Promise(resolve=>server.close(resolve))));
      input.destroy();output.destroy();
    }
  `], { stdout: "pipe", stderr: "pipe", timeout: 5000 });
  expect({ exitCode: child.exitCode, stderr: child.stderr.toString() }).toEqual({ exitCode: 0, stderr: "" });
});

test("noninteractive conflicts fail promptly and unrelated startup errors are not retried", async () => {
  const occupied = await listen();
  const streams = { input: new PassThrough(), output: new PassThrough() };
  try {
    await expect(startCLIListener("Service", { host: "127.0.0.1", port: port(occupied) },
      address => listen(address.host, address.port), { signal: new AbortController().signal, streams })).rejects.toBeInstanceOf(AddressConflict);
    const denied = Object.assign(new Error("Permission denied"), { code: "EACCES" });
    await expect(startCLIListener("Service", { host: "127.0.0.1", port: 3001 },
      async () => { throw denied; }, { signal: new AbortController().signal, streams })).rejects.toBe(denied);
  } finally { await close(occupied); }
});
