import { expect } from "bun:test";

export async function expectCompilerError(operation: Promise<unknown>, message: string): Promise<void> {
  // Await compiler RPC before matching: Bun's asynchronous rejection matcher can
  // starve child-process I/O, turning millisecond compiler work into long stalls.
  const failure: unknown = await operation.then(() => undefined, error => error);
  expect(failure).toBeInstanceOf(Error);
  expect(failure instanceof Error ? failure.message : undefined).toContain(message);
}
