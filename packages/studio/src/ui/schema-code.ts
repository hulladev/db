let highlighter: Promise<typeof import("./syntax.ts")> | undefined;
let pending: Promise<void> | undefined;

const blocks = (root: HTMLElement) => [...root.querySelectorAll<HTMLElement>("pre code.language-json")]
  .filter(block => !block.dataset.highlighted && (block.textContent?.length ?? 0) <= 128 * 1024);

/** Plain text is already visible; optional highlighting never blocks navigation. */
export function highlightSchema(root: HTMLElement): Promise<void> {
  if (pending) return pending;
  if (!blocks(root).length) return Promise.resolve();
  pending = (async () => {
    try {
      const { highlightJSON } = await (highlighter ??= import("./syntax.ts"));
      // Query after loading and coalesce requests so table switches neither keep
      // detached schema nodes alive nor decorate a table that was replaced.
      for (const block of blocks(root)) {
        if (!block.isConnected) continue;
        // Highlight.js escapes source text before adding its own token spans.
        block.innerHTML = highlightJSON(block.textContent ?? "");
        block.dataset.highlighted = "true";
      }
    } catch {
      highlighter = undefined;
      // Network/CSP/load failures retain the original selectable schema text.
    } finally {
      pending = undefined;
    }
  })();
  return pending;
}
