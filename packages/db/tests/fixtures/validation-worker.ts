import { installWorker } from "../../src/worker.ts";
import schema from "./deployed-schema.ts";
import * as validation from "./validation.ts";

installWorker({ schema, modules: { validation } });
