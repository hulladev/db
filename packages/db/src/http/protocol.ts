import type { Operation } from "../contracts.ts";
import { DatabaseError, applicationError, type DatabaseErrorOptions } from "../error.ts";
export interface Session { readonly scope: string; readonly principal: string; readonly authorizationScope: string }
export interface Snapshot {
  readonly source: string; readonly scope: string; readonly principal: string; readonly authorization_scope: string;
  readonly revision: string; readonly result: unknown; readonly page?: { readonly range: string };
}
export interface Head extends Omit<Snapshot, "result"> {}
export interface Receipt { readonly request_id: string; readonly source: string; readonly scope: string; readonly revision: string; readonly result: unknown }
export interface Round extends Head { readonly results: readonly ({ readonly key: string; readonly result: unknown } | { readonly key: string; readonly error: { readonly code: string } })[] }
export interface Delivery { readonly subscription: string; readonly sequence: string; readonly round: Round }
export const uuidPattern = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
export function uuid(value: unknown): string { if (typeof value !== "string" || !uuidPattern.test(value)) throw new TypeError("Invalid UUID"); return value; }
export function sequence(value: unknown): string {
  if (typeof value !== "string" || !/^(0|[1-9][0-9]*)$/.test(value) || BigInt(value) > (1n << 63n) - 1n) throw new TypeError("Invalid sequence");
  return value;
}
export function record(value: unknown, keys: readonly string[]): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)
    || Object.keys(value).some(key => !keys.includes(key)) || keys.some(key => !Object.hasOwn(value, key))) throw new TypeError("Invalid response envelope");
  return value as Record<string, unknown>;
}
export function parseHead(value: unknown): Head {
  const h = record(value, ["source", "scope", "principal", "authorization_scope", "revision"]);
  return { source: uuid(h.source), scope: uuid(h.scope), principal: uuid(h.principal), authorization_scope: uuid(h.authorization_scope), revision: sequence(h.revision) };
}
export interface Ticket { readonly id: string; readonly expires_in_ms: number; readonly renew_after_ms: number }
export function parseSession(value: unknown): Head & { readonly ticket: Ticket } {
  const { ticket, ...head } = record(value, ["source", "scope", "principal", "authorization_scope", "revision", "ticket"]);
  const t = record(ticket, ["id", "expires_in_ms", "renew_after_ms"]);
  if (!Number.isSafeInteger(t.expires_in_ms) || !Number.isSafeInteger(t.renew_after_ms)
    || (t.renew_after_ms as number) < 1 || (t.expires_in_ms as number) <= (t.renew_after_ms as number)) throw new TypeError("Invalid ticket duration");
  return { ...parseHead(head), ticket: { id: uuid(t.id), expires_in_ms: t.expires_in_ms as number, renew_after_ms: t.renew_after_ms as number } };
}
export function parseSnapshot(value: unknown): Snapshot {
  const s = record(value, ["source", "scope", "principal", "authorization_scope", "revision", "result", ...(typeof value === "object" && value !== null && Object.hasOwn(value, "page") ? ["page"] : [])]);
  const { result, page, ...head } = s;
  return { ...parseHead(head), result, ...(page === undefined ? {} : { page: parsePage(page) }) };
}
export function parseReceipt(value: unknown, requestId: string, session: Session, source: string): Receipt {
  const r = record(value, ["request_id", "source", "scope", "revision", "result"]);
  if (uuid(r.request_id) !== requestId || uuid(r.scope) !== session.scope || uuid(r.source) !== source) throw new DatabaseError({ code: "reset_required", message: "reset_required" });
  return { request_id: requestId, scope: session.scope, source, revision: sequence(r.revision), result: r.result };
}
export function parseError(value: unknown): DatabaseError {
  const hasApplication = typeof value === "object" && value !== null && Object.hasOwn(value, "application");
  const r = record(value, hasApplication ? ["code", "application"] : ["code"]);
  if (hasApplication) {
    if (r.code !== "execution_failed") throw new TypeError("Invalid application error envelope");
    const raw = r.application;
    const payload = record(raw, ["code", "message", ...(typeof raw === "object" && raw !== null && Object.hasOwn(raw, "details") ? ["details"] : [])]);
    const error = applicationError(payload as unknown as DatabaseErrorOptions);
    if (new TextEncoder().encode(JSON.stringify(error)).length > 16 * 1024) throw new TypeError("Application error exceeds budget");
    return error;
  }
  const codes: readonly string[] = ["unauthorized", "forbidden", "invalid_request", "not_found", "conflict", "execution_limit", "execution_failed", "row_expectation_failed", "quota_exceeded", "ticket_expired", "request_id_reused", "receipt_expired", "capacity", "unavailable", "reset_required", "outcome_unknown"];
  if (typeof r.code !== "string" || !codes.includes(r.code)) throw new TypeError("Invalid remote error");
  return new DatabaseError({ code: r.code, message: r.code });
}
export function canonical(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (typeof value === "object" && value !== null) return `{${Object.entries(value).sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0).map(([key, v]) => `${JSON.stringify(key)}:${canonical(v)}`).join(",")}}`;
  const encoded = JSON.stringify(value);
  if (encoded === undefined) throw new TypeError("Value is not a wire value");
  return encoded;
}
export function operationKey(operation: Operation): string {
  if (!operation.codecs) throw new TypeError("HTTP functions require generated value codecs");
  return canonical([operation.name, operation.codecs.input.encode(operation.input), ...(operation.page ? [operation.page.owner] : [])]);
}

export function parsePage(value: unknown): { range: string } {
  const page = record(value, ["range"]);
  if (typeof page.range !== "string" || !page.range || page.range.length > 65536) throw new TypeError("Invalid page range");
  return { range: page.range };
}
