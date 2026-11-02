import { style } from "@hulla/style";

// Native DOM adaptations of Hulla UI variants; no framework runtime is needed.
export const { cn, vn } = style();
const $button = vn({ primary: "primary", secondary: "secondary", ghost: "ghost" });
export const buttonClass = (variant: typeof $button.infer = "ghost") => cn("button", $button(variant));
const types = { string: "type-text", uuid: "type-key", int64: "type-number", float64: "type-number", boolean: "type-boolean", json: "type-json", date: "type-date", timestamp: "type-date", localTimestamp: "type-date" };
const $type = vn(types);
export const typeClass = (scalar: string) => cn("field-type", $type(Object.hasOwn(types, scalar) ? scalar as keyof typeof types : "string"));
export const typeSymbol = (scalar: string): string => ({ string: "Aa", uuid: "⌑", int64: "#", float64: "#", boolean: "◐", json: "{}", date: "◷", timestamp: "◷", localTimestamp: "◷" })[scalar] ?? "Aa";
export function icon(name: string): SVGSVGElement {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("class", "icon"); svg.setAttribute("aria-hidden", "true");
  const use = document.createElementNS("http://www.w3.org/2000/svg", "use");
  use.setAttribute("href", `#i-${name}`); svg.append(use); return svg;
}
