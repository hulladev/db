import { installWorker } from "../../src/worker.ts";
import schema from "./schema.ts";
import { environment } from "./typescript.ts";

installWorker({ schema, modules: { typescript: { environment } } });
