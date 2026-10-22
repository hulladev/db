import { existsSync } from "node:fs";
import { executables, nativePlatforms, verifyNative } from "./native-artifacts.ts";

let platforms = 0;
for (const platform of nativePlatforms) {
  const directory = new URL(`../native/${platform}/`, import.meta.url);
  if (!existsSync(directory)) continue;
  for (const name of executables) await verifyNative(new URL(name, directory), platform);
  platforms++;
}
if (!platforms) throw new Error("No native artifacts staged. Run bun run build:native on a supported host before packing.");
