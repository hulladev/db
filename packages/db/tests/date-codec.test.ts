import { expect, test } from "bun:test";
import { valueCodec } from "../src/codec.ts";

test("Date codecs preserve milliseconds, isolate mutable instances and distinguish literal strings", () => {
  const codec = valueCodec({ kind: "union", choices: [{ kind: "date" }, { kind: "string" }] });
  const input = new Date("2026-10-20T12:34:56.789Z");
  const encoded = codec.encode(input);
  expect(encoded).toEqual({ $date: 1792499696789 });
  const decoded = codec.decode(encoded) as Date;
  expect(decoded).toBeInstanceOf(Date);
  expect(decoded.toISOString()).toBe("2026-10-20T12:34:56.789Z");
  decoded.setTime(0);
  expect(input.toISOString()).toBe("2026-10-20T12:34:56.789Z");
  expect((codec.parse(input) as Date)).not.toBe(input);
  expect(codec.decode("2026-10-20T12:34:56.789Z")).toBe("2026-10-20T12:34:56.789Z");
  for (const invalid of [new Date(NaN), Object.assign(new Date(0), { hidden: true }), new URL("https://example.test")]) {
    expect(() => valueCodec({ kind: "date" }).encode(invalid)).toThrow();
  }
});
