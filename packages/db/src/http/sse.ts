export interface SseEvent { readonly event: string; readonly id: string; readonly data: string }

/** Incremental UTF-8 SSE parsing with bounded frames and native stream backpressure. */
export async function* sseEvents(body: ReadableStream<Uint8Array>, maxBytes: number, activity: () => void = () => {}, signal?: AbortSignal): AsyncGenerator<SseEvent> {
  const reader = body.getReader();
  const cancel = () => { void reader.cancel().catch(() => {}); };
  signal?.addEventListener("abort", cancel, { once: true });
  const decoder = new TextDecoder("utf-8", { fatal: true });
  let line = "", event = "message", id = "", data: string[] = [], bytes = 0, pendingCR = false;
  function consume(text: string): SseEvent | undefined {
    if (!text) {
      const frame = data.length ? { event, id, data: data.join("\n") } : undefined;
      event = "message"; id = ""; data = []; bytes = 0;
      return frame;
    }
    if (text.startsWith(":")) return;
    const colon = text.indexOf(":");
    const field = colon < 0 ? text : text.slice(0, colon);
    const value = colon < 0 ? "" : text.slice(colon + 1).replace(/^ /, "");
    if (field === "event") event = value;
    if (field === "id") { if (value.includes("\0")) throw new TypeError("Invalid SSE ID"); id = value; }
    if (field === "data") data.push(value);
  }
  try {
    signal?.throwIfAborted();
    while (true) {
      const chunk = await reader.read();
      signal?.throwIfAborted();
      activity();
      if (chunk.done) {
        decoder.decode();
        if (line || data.length) throw new TypeError("Truncated SSE frame");
        return;
      }
      for (const character of decoder.decode(chunk.value, { stream: true })) {
        if (pendingCR && character === "\n") { pendingCR = false; continue; }
        pendingCR = character === "\r";
        bytes += new TextEncoder().encode(character).length;
        if (bytes > maxBytes) throw new TypeError("SSE frame exceeds budget");
        if (character === "\n" || character === "\r") {
          const frame = consume(line); line = "";
          if (frame) yield frame;
        } else { line += character; }
      }
    }
  } finally { signal?.removeEventListener("abort", cancel); cancel(); reader.releaseLock(); }
}
