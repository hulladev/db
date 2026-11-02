import type { StudioTable } from "../types.ts";
import { buttonClass, icon, typeClass, typeSymbol } from "./style.ts";
import { layoutSchema } from "./graph-layout.ts";

function node<K extends keyof HTMLElementTagNameMap>(tag: K, className: string, text?: string): HTMLElementTagNameMap[K] {
  const value = document.createElement(tag); value.className = className;
  if (text !== undefined) value.textContent = text;
  return value;
}
export function createDiagram(tables: readonly StudioTable[], open: (table: StudioTable) => void): { root: HTMLElement; fit(): void } {
  const layout = layoutSchema(tables), root = node("div", "diagram");
  const viewport = node("div", "diagram-viewport"); viewport.tabIndex = 0;
  viewport.setAttribute("aria-label", "Table relationship diagram. Scroll to pan, or use the zoom controls.");
  const sizer = node("div", "diagram-sizer"), world = node("div", "diagram-world");
  world.style.width = `${layout.width}px`; world.style.height = `${layout.height}px`;
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("class", "diagram-connections"); svg.setAttribute("width", String(layout.width)); svg.setAttribute("height", String(layout.height)); svg.setAttribute("aria-hidden", "true");
  const defs = document.createElementNS(svg.namespaceURI, "defs"), marker = document.createElementNS(svg.namespaceURI, "marker");
  marker.setAttribute("id", "reference-arrow"); marker.setAttribute("viewBox", "0 0 10 10"); marker.setAttribute("refX", "9"); marker.setAttribute("refY", "5"); marker.setAttribute("markerWidth", "6"); marker.setAttribute("markerHeight", "6"); marker.setAttribute("orient", "auto-start-reverse");
  const arrow = document.createElementNS(svg.namespaceURI, "path"); arrow.setAttribute("d", "M0 0L10 5L0 10Z"); marker.append(arrow); defs.append(marker); svg.append(defs);
  for (const edge of layout.edges) {
    const path = document.createElementNS(svg.namespaceURI, "path"); path.setAttribute("d", edge.path); path.setAttribute("class", "relationship-line"); path.setAttribute("marker-end", "url(#reference-arrow)");
    svg.append(path);
  }
  world.append(svg);
  for (const item of layout.nodes) {
    const card = node("article", "diagram-node"); card.style.left = `${item.x}px`; card.style.top = `${item.y}px`; card.style.width = `${item.width}px`;
    const header = node("button", "diagram-node-title"); header.type = "button"; header.setAttribute("aria-label", `Open table ${item.table.name}`);
    header.append(icon("table"), node("strong", "", item.table.name), node("span", "node-field-count", String(item.table.fields.length)));
    card.append(header);
    for (const field of item.fields) {
      const row = node("div", "diagram-field"), symbol = node("span", typeClass(field.scalar), typeSymbol(field.scalar)); symbol.setAttribute("aria-hidden", "true");
      row.append(symbol, node("span", "diagram-field-name", field.name), node("small", "", field.scalar));
      if (field.name === item.table.primaryKey) { const key = icon("key"); key.setAttribute("class", "icon primary-key-icon"); row.append(key); }
      else if (field.reference) row.append(node("span", "foreign-key-label", "FK"));
      row.title = field.reference ? `${field.name} → ${field.reference} · on delete ${field.onDelete ?? "restrict"}` : `${field.name}: ${field.scalar}${field.nullable ? " · nullable" : ""}`;
      card.append(row);
    }
    card.append(node("div", "diagram-node-footer", item.omitted ? `+ ${item.omitted} more fields · Open table` : "Open table →"));
    card.addEventListener("click", () => open(item.table)); world.append(card);
  }
  if (!tables.length) world.append(node("p", "diagram-empty", "No tables are defined in this schema."));
  sizer.append(world); viewport.append(sizer); root.append(viewport);
  const tools = node("div", "diagram-tools"), minus = node("button", buttonClass("secondary"), "−"), plus = node("button", buttonClass("secondary"), "+");
  minus.setAttribute("aria-label", "Zoom out"); plus.setAttribute("aria-label", "Zoom in");
  const percent = node("span", "diagram-zoom", "100%"), fit = node("button", buttonClass("secondary"), "Fit"); fit.setAttribute("aria-label", "Fit diagram");
  tools.append(minus, percent, plus, fit); root.append(tools);
  const legend = node("div", "diagram-legend"); legend.append(icon("key"), node("span", "", "Primary key"), node("span", "foreign-key-label", "FK"), node("span", "", "Reference → primary key")); root.append(legend);
  let scale = 1, fitted = true;
  function zoom(next: number) {
    const centerX = (viewport.scrollLeft + viewport.clientWidth / 2) / scale, centerY = (viewport.scrollTop + viewport.clientHeight / 2) / scale;
    scale = Math.max(.25, Math.min(1.5, next)); world.style.transform = `scale(${scale})`;
    sizer.style.width = `${layout.width * scale}px`; sizer.style.height = `${layout.height * scale}px`;
    percent.textContent = `${Math.round(scale * 100)}%`;
    viewport.scrollLeft = centerX * scale - viewport.clientWidth / 2; viewport.scrollTop = centerY * scale - viewport.clientHeight / 2;
  }
  function fitView() {
    if (!viewport.clientWidth || !viewport.clientHeight) return;
    fitted = true; zoom(Math.min(1, Math.max(.25, Math.min(viewport.clientWidth / layout.width, viewport.clientHeight / layout.height))));
    viewport.scrollLeft = 0; viewport.scrollTop = 0;
  }
  plus.addEventListener("click", () => { fitted = false; zoom(scale + .15); }); minus.addEventListener("click", () => { fitted = false; zoom(scale - .15); }); fit.addEventListener("click", fitView);
  // One diagram lives for the document lifetime; navigation only hides it.
  new ResizeObserver(() => { if (fitted) fitView(); }).observe(viewport);
  zoom(1);
  return { root, fit: fitView };
}
