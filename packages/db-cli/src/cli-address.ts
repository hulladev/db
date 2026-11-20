import { createServer } from "node:net";
import { createInterface } from "node:readline";
import type { Readable, Writable } from "node:stream";

export interface ListenAddress { readonly host: string; readonly port: number }
export interface RememberedAddress { readonly requested: ListenAddress; readonly address: ListenAddress }
export type DevelopmentAddresses = Partial<Record<"service" | "studio", RememberedAddress>>;
export class StartupCancelled extends Error {
  constructor() { super("Startup cancelled."); }
}
export class AddressConflict extends Error {}
interface PromptStreams {
  readonly input: Readable & { readonly isTTY?: boolean };
  readonly output: Writable & { readonly isTTY?: boolean };
}
interface AddressOptions {
  readonly signal: AbortSignal;
  readonly fixedHost?: boolean;
  readonly streams?: PromptStreams;
}

export function addressURL({ host, port }: ListenAddress): string {
  return `http://${host.includes(":") ? `[${host}]` : host}:${port}`;
}

function inUse(error: unknown): boolean {
  return !!error && typeof error === "object" && "code" in error && error.code === "EADDRINUSE";
}

async function nextAvailable(address: ListenAddress, signal: AbortSignal): Promise<ListenAddress> {
  for (let port = address.port + 1; port <= 65535; port++) {
    signal.throwIfAborted();
    const server = createServer();
    try {
      await new Promise<void>((resolve, reject) => {
        server.once("error", reject);
        server.listen({ host: address.host, port, exclusive: true }, resolve);
      });
      return { host: address.host, port };
    } catch (error) { if (!inUse(error)) throw error; }
    finally {
      if (server.listening) await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
    }
  }
  throw new Error(`No available port after ${address.port}; enter a lower port instead.`);
}

function customAddress(value: string, current: ListenAddress, fixedHost: boolean): ListenAddress {
  let host = current.host, port: number;
  if (/^\d+$/.test(value)) port = Number(value);
  else {
    if (!/^(?:http:\/\/)?(?:\[[\da-fA-F:]+\]|[a-zA-Z\d.-]+):\d+\/?$/.test(value)) throw new Error("Enter a port or an HTTP address such as 127.0.0.1:3002.");
    const url = new URL(value.startsWith("http://") ? value : `http://${value}`);
    host = url.hostname.replace(/^\[|\]$/g, "");
    port = Number(url.port || 80);
  }
  if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error("Enter a port from 1 to 65535.");
  if (fixedHost && host !== current.host) throw new Error(`This listener must stay on ${current.host}; enter a different port.`);
  return { host, port };
}

async function chooseAddress(label: string, address: ListenAddress, options: AddressOptions): Promise<ListenAddress> {
  const { input, output } = options.streams ?? { input: process.stdin, output: process.stderr };
  if (!input.isTTY || !output.isTTY) {
    throw new AddressConflict(`${label} address ${addressURL(address)} is in use. Choose another port in your config or use --studio-port for Studio. Run in a terminal to choose interactively.`);
  }
  // readline supports cancellable input in both the Node CLI and Bun workers.
  const readline = createInterface({ input, output, terminal: true });
  const closed = new AbortController();
  const signal = AbortSignal.any([options.signal, closed.signal]);
  let ended = false;
  readline.once("close", () => { ended = true; closed.abort(); });
  const cancel = () => readline.close();
  readline.on("SIGINT", cancel);
  const ask = (question: string): Promise<string> => new Promise((resolve, reject) => {
    if (ended || signal.aborted) { reject(new StartupCancelled()); return; }
    const cleanup = () => {
      readline.off("close", onClose);
      signal.removeEventListener("abort", cancel);
    };
    const onClose = () => { cleanup(); reject(new StartupCancelled()); };
    readline.once("close", onClose);
    signal.addEventListener("abort", cancel, { once: true });
    readline.question(question, answer => { cleanup(); resolve(answer.trim()); });
  });
  try {
    output.write(`\n  ${label} address ${addressURL(address)} is already in use.\n\n`);
    output.write("  1) Use the next available port (default)\n  2) Enter a custom address\n  3) Exit\n\n");
    while (true) {
      const choice = await ask("  Choose [1]: ");
      if (!choice || choice === "1") {
        try {
          const next = await nextAvailable(address, signal);
          if (signal.aborted) throw new StartupCancelled();
          return next;
        }
        catch (error) {
          if (signal.aborted) throw new StartupCancelled();
          output.write(`  ${error instanceof Error ? error.message : "Cannot find an available port."}\n`);
        }
      } else if (choice === "2") {
        while (true) {
          const value = await ask(`  ${options.fixedHost ? "Port" : "Address or port"} (blank to go back): `);
          if (!value) break;
          try { return customAddress(value, address, !!options.fixedHost); }
          catch (error) { output.write(`  ${error instanceof Error ? error.message : "Invalid address."}\n`); }
        }
      } else if (choice === "3") throw new StartupCancelled();
      else output.write("  Choose 1, 2, or 3. Press Enter to use the next available port.\n");
    }
  } finally { readline.close(); }
}

/** Retry only real bind conflicts; other startup failures retain their original cause. */
export async function startCLIListener<T>(label: string, requested: ListenAddress,
  start: (address: ListenAddress) => Promise<T>, options: AddressOptions): Promise<{ listener: T; address: ListenAddress }> {
  let address = requested;
  while (true) {
    options.signal.throwIfAborted();
    try { return { listener: await start(address), address }; }
    catch (error) {
      if (!inUse(error)) throw error;
      address = await chooseAddress(label, address, options);
    }
  }
}
