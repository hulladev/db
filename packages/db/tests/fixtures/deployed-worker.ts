import { installWorker } from "../../src/worker.ts";
import schema from "./deployed-schema.ts";
import * as deployed from "./deployed.ts";
import * as typescript from "./typescript.ts";

installWorker({ schema, modules: { deployed, typescript } });
