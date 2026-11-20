#!/usr/bin/env node
import { startService } from "./service.ts";

async function serve(config: string) {
  let service: Awaited<ReturnType<typeof startService>> | undefined;
  let stopping = false;
  const stop = () => { stopping = true; void service?.close().catch(() => {}); };
  process.on("SIGINT", stop);
  process.on("SIGTERM", stop);
  try {
    service = await startService(config);
    if (!stopping) console.log(`Database service listening at ${service.url}`);
    else await service.close();
    await service.closed;
  } finally {
    process.off("SIGINT", stop);
    process.off("SIGTERM", stop);
  }
}

const [config = "db.config.ts", ...args] = process.argv.slice(2);
if (config === "--help" || config === "-h") console.log("Usage: hulla-db-serve [entrypoint]");
else if (config.startsWith("-") || args.length) {
  console.error("Usage: hulla-db-serve [entrypoint]");
  process.exitCode = 1;
} else {
  try { await serve(config); }
  catch (error) {
    console.error(error instanceof Error ? error.message : "Database service failed");
    process.exitCode = 1;
  }
}
