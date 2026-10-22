import { dirname, resolve } from "node:path";
import { defineConfig } from "./config.ts";
import { generateProject } from "./project-codegen.ts";
import { startService } from "./service.ts";

let stopping = false;
let service: Awaited<ReturnType<typeof startService>> | undefined;
const stop = () => { stopping = true; void service?.close().catch(() => {}); };
process.on("SIGINT", stop); process.on("SIGTERM", stop);
try {
  const path = resolve(process.argv[2]!);
  const config = defineConfig((await import(Bun.pathToFileURL(path).href)).default);
  const schema = resolve(dirname(path), config.schema ?? "db/schema.ts");
  process.send?.({ watch: [dirname(schema), ...(config.functions ? [resolve(dirname(path), config.functions)] : [])] });
  await generateProject(path);
  if (!stopping) {
    service = await startService(path);
    if (stopping) await service.close();
    else {
      console.log(`Database service listening at ${service.url}`);
      await service.closed;
    }
  }
} catch (error) {
  if (!stopping) {
    console.error(error instanceof Error ? error.message : "Development startup failed");
    console.error("Waiting for source changes before restarting.");
    process.exitCode = 1;
  }
} finally {
  process.off("SIGINT", stop); process.off("SIGTERM", stop);
  await service?.close().catch(() => {});
  if (process.connected) process.disconnect?.();
}
