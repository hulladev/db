import { expect, test } from "bun:test";
import { DatabaseError, isApplicationError, type DatabaseErrorDetails, type DatabaseErrorOptions } from "../src/error.ts";
import { parseError } from "../src/http/protocol.ts";

test("intentional application errors serialize only their declared public payload", () => {
  const error = new DatabaseError({ code: "TASK_LOCKED", message: "This task can no longer be edited.", details: { taskId: "task-1", reasons: ["archived", null] } });
  Object.defineProperty(error, "cause", { value: new Error("postgres://secret@driver") });
  error.stack = "private server stack";
  expect(error).toBeInstanceOf(Error);
  expect(JSON.parse(JSON.stringify(error))).toEqual({ code: "TASK_LOCKED", message: "This task can no longer be edited.", details: { taskId: "task-1", reasons: ["archived", null] } });
  expect(JSON.parse(JSON.stringify(new DatabaseError({ code: "FAILED", message: "Failed" })))).toEqual({ code: "FAILED", message: "Failed" });
});

test("error details preserve JSON object keys and cannot change after validation", () => {
  const details = JSON.parse('{"__proto__":{"reason":"locked"},"nested":[{"id":1}]}');
  const error = new DatabaseError({ code: "TASK_LOCKED", message: "Locked", details });
  details.nested[0].id = Infinity;
  details.extra = new Error("secret");
  expect(JSON.parse(JSON.stringify(error)).details).toEqual(JSON.parse('{"__proto__":{"reason":"locked"},"nested":[{"id":1}]}'));
  expect(() => { (error.details as { nested: { id: number }[] }).nested[0]!.id = 2; }).toThrow(TypeError);
});

test("details reject values that JSON would silently omit, convert, or lose", () => {
  const cycle: Record<string, unknown> = {};
  cycle.self = cycle;
  const accessor = Object.defineProperty({}, "secret", { enumerable: true, get() { throw new Error("Getter ran"); } });
  const invalid = [undefined, () => 1, Symbol("secret"), 1n, NaN, Infinity, Number.MAX_SAFE_INTEGER + 1, "\ud800", new Error("driver"), new Date(), new Map(), cycle, [ , 1], accessor, { [Symbol("secret")]: "hidden" }, Object.defineProperty({}, "secret", { value: "hidden" }), { toJSON: () => "converted" }];
  for (const value of invalid) {
    // An absent top-level details value is allowed; nested undefined is not.
    expect(() => new DatabaseError({ code: "INVALID", message: "Invalid", details: { value } as DatabaseErrorDetails })).toThrow(TypeError);
  }
});

test("construction requires explicit code and message", () => {
  for (const options of [{ message: "Failure" }, { code: "FAILURE" }, { code: "", message: "Failure" }, { code: 42, message: "Failure" }, { code: "FAILURE", message: new Error("private") }]) {
    expect(() => new DatabaseError(options as DatabaseErrorOptions)).toThrow(TypeError);
  }
});

test("remote errors separate intentional payloads from sanitized transport failures", () => {
  const error = parseError({ code: "conflict" });
  expect(error).toBeInstanceOf(DatabaseError);
  expect(error.toJSON()).toEqual({ code: "conflict", message: "conflict" });
  expect(parseError({ code: "outcome_unknown" }).code).toBe("outcome_unknown");
  expect(parseError({ code: "execution_limit" }).toJSON()).toEqual({ code: "execution_limit", message: "execution_limit" });
  for (const code of ["TASK_LOCKED", "unauthorized", "outcome_unknown"]) {
    const application = parseError({ code: "execution_failed", application: { code, message: "Cannot edit", details: null } });
    expect(application.toJSON()).toEqual({ code, message: "Cannot edit", details: null });
    expect(isApplicationError(application)).toBe(true);
  }
  expect(() => parseError({ code: "unauthorized", application: { code: "X", message: "leak" } })).toThrow();
  expect(() => parseError({ code: "execution_failed", application: { code: "X", message: "x".repeat(16384) } })).toThrow();
  for (const value of [{ code: "TASK_LOCKED" }, { code: "conflict", message: "private driver error" }, { code: "conflict", details: { password: "secret" } }, { code: "conflict", stack: "private" }, { code: 1 }, {}, null, []]) {
    expect(() => parseError(value)).toThrow(TypeError);
  }
});
