import type { StudioDeleteResult, StudioField, StudioPage, StudioRow, StudioSchema, StudioTable } from "./types.ts";
import { buttonClass, cn, icon, typeClass, typeSymbol } from "./ui/style.ts";
import { highlightSchema } from "./ui/schema-code.ts";
import { createDiagram } from "./ui/diagram.ts";
import { createCatalog } from "./ui/catalog.ts";
import { indexCount, relationships, ruleCount } from "./ui/graph-layout.ts";

function element<T extends HTMLElement>(id: string): T {
  const value = document.getElementById(id);
  if (!value) throw new Error(`Missing Studio element ${id}`);
  return value as T;
}
function node<K extends keyof HTMLElementTagNameMap>(tag: K, text?: string, className?: string): HTMLElementTagNameMap[K] {
  const value = document.createElement(tag);
  if (text !== undefined) value.textContent = text;
  if (className) value.className = cn(className);
  return value;
}
const display = (value: unknown): string => value === null ? "null" : typeof value === "object" ? JSON.stringify(value) : String(value);
const pretty = (value: unknown) => JSON.stringify(value, null, 2);
const dialog = element<HTMLDialogElement>("editor");
const deleteDialog = element<HTMLDialogElement>("delete-dialog");
const notice = element("notice");
let schema: StudioSchema;
let selected: StudioTable | undefined;
let rows: readonly StudioRow[] = [];
let filters: { field: string; operator: string; value: unknown }[] = [];
let sort: { field: string; direction: "asc" | "desc" } | undefined;
let cursors: (string | null)[] = [null], pageIndex = 0, nextCursor: string | null = null;
let generation = 0, loading = false, saving = false;
let pageView: "map" | "catalog" | "table" = "map", dataLoaded = false;
let diagram: ReturnType<typeof createDiagram> | undefined;
const selection = new Map<string, StudioRow>();
let lastSelectionIndex: number | undefined;
let inspected: StudioRow | undefined;
let pendingDeletion: { table: StudioTable; rows: StudioRow[] } | undefined;
let editors: { field: StudioField; input: HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement; nullable?: HTMLInputElement }[] = [];

const fragment = new URLSearchParams(location.hash.slice(1));
const incoming = fragment.get("token");
if (incoming) { sessionStorage.setItem("hulla-studio-token", incoming); history.replaceState(null, "", location.pathname); }
const token = sessionStorage.getItem("hulla-studio-token") ?? "";
// Opening a new terminal link in the same tab can be a fragment-only navigation.
window.addEventListener("hashchange", () => { if (new URLSearchParams(location.hash.slice(1)).has("token")) location.reload(); });
async function api<T>(path: string, input?: unknown): Promise<T> {
  const response = await fetch(`/api/${path}`, {
    method: input === undefined ? "GET" : "POST",
    headers: { Authorization: `Bearer ${token}`, ...(input === undefined ? {} : { "Content-Type": "application/json" }) },
    ...(input === undefined ? {} : { body: JSON.stringify(input) }),
    signal: AbortSignal.timeout(15_000),
  });
  const result = await response.json();
  if (!response.ok) throw new Error(result.error ?? "Studio request failed");
  return result as T;
}
function message(error?: unknown) {
  notice.hidden = error === undefined;
  notice.textContent = error instanceof Error ? error.message : error === undefined ? "" : String(error);
}
function navigation() {
  const search = element<HTMLInputElement>("table-search").value.toLowerCase();
  element("tables").replaceChildren(...schema.tables.filter(table => table.name.toLowerCase().includes(search)).map(table => {
    const button = node("button");
    button.append(icon("table"), node("span", table.name, "table-name"));
    button.setAttribute("aria-current", String(selected?.name === table.name));
    button.addEventListener("click", () => choose(table));
    return button;
  }));
}
function displayPage(page: typeof pageView) {
  pageView = page;
  element("map-page").hidden = page !== "map";
  element("catalog-page").hidden = page !== "catalog";
  element("table-detail").hidden = page !== "table";
  for (const name of ["map", "catalog"]) {
    const button = element(`nav-${name}`);
    if (page === name) button.setAttribute("aria-current", "page"); else button.removeAttribute("aria-current");
  }
}
function workspace(page: "map" | "catalog", push = true) {
  if (saving) return;
  generation++; loading = false; dataLoaded = false; selected = undefined; selection.clear(); lastSelectionIndex = undefined;
  dialog.close(); element<HTMLDialogElement>("delete-dialog").close(); message();
  displayPage(page); navigation();
  element("breadcrumb").textContent = page === "map" ? "Schema map" : "Schema catalog";
  if (push) history.pushState(null, "", `#${page}`);
  if (page === "map") requestAnimationFrame(() => diagram?.fit());
}
function choose(table: StudioTable, tab: "data" | "schema" = "data", push = true) {
  if (saving) return;
  generation++; loading = false; dataLoaded = false;
  dialog.close(); selected = table; filters = []; sort = undefined; selection.clear(); lastSelectionIndex = undefined;
  displayPage("table"); message();
  element("edit-hint").textContent = schema.readOnly ? "Select rows to export JSON, or open a row to inspect it." : "Select rows for bulk actions, or open a row to edit it. Selection stays on the current page.";
  element("table-title").textContent = table.name;
  element("breadcrumb").textContent = table.name;
  const indexes = table.indexes instanceof Array ? table.indexes.length : 0;
  element("table-description").textContent = `${table.fields.length} fields · ${indexes} ${indexes === 1 ? "index" : "indexes"} · ${table.primaryKey} primary key`;
  element<HTMLButtonElement>("refresh").disabled = false;
  element<HTMLSelectElement>("filter-field").replaceChildren(...table.fields.map(field => {
    const option = node("option", field.name); option.value = field.name; return option;
  }));
  cursors = [null]; pageIndex = 0; nextCursor = null; rows = [];
  element("schema-view").hidden = tab !== "schema";
  filterOperators(); navigation(); renderSchema(table); chips(); renderRows(); controls();
  if (push) history.pushState(null, "", `#table=${encodeURIComponent(table.name)}&view=${tab}`);
  showTab(tab);
}
function route() {
  const hash = new URLSearchParams(location.hash.slice(1)), name = hash.get("table");
  const table = schema.tables.find(table => table.name === name);
  if (table) choose(table, hash.get("view") === "schema" ? "schema" : "data", false);
  else workspace(hash.has("catalog") ? "catalog" : "map", false);
}
function filterOperators() {
  const field = selected?.fields.find(field => field.name === element<HTMLSelectElement>("filter-field").value);
  const operators = ["=", "!=", ...(field?.scalar === "json" || field?.scalar === "boolean" ? [] : [">", ">=", "<", "<="]), ...(field?.scalar === "string" ? ["contains"] : []), ...(field?.nullable ? ["is", "is not"] : [])];
  element<HTMLSelectElement>("filter-operator").replaceChildren(...operators.map(operator => {
    const option = node("option", operator === "is" ? "is null" : operator === "is not" ? "is not null" : operator);
    option.value = operator; return option;
  }));
  element<HTMLInputElement>("filter-value").disabled = false;
}
function parsed(field: StudioField, text: string): unknown {
  if (field.scalar === "json") return JSON.parse(text);
  if (field.scalar === "boolean") {
    if (text !== "true" && text !== "false") throw new Error(`${field.name} requires true or false`);
    return text === "true";
  }
  if (field.scalar === "float64") {
    if (!text.trim() || !Number.isFinite(Number(text))) throw new Error(`${field.name} requires a finite number`);
    return Number(text);
  }
  return text;
}
function chips() {
  element("filter-chips").replaceChildren(...filters.map((filter, index) => {
    const chip = node("span", `${filter.field} ${filter.operator} ${display(filter.value)}`, "chip");
    const remove = node("button", undefined, buttonClass()); remove.append(icon("close"));
    remove.setAttribute("aria-label", `Remove filter on ${filter.field}`);
    remove.addEventListener("click", () => { filters.splice(index, 1); reset(); });
    chip.append(remove); return chip;
  }));
}
function reset() { cursors = [null]; pageIndex = 0; chips(); void load(); }
function controls() {
  element<HTMLButtonElement>("previous").disabled = saving || loading || pageIndex === 0;
  element<HTMLButtonElement>("next").disabled = saving || loading || nextCursor === null;
  element<HTMLButtonElement>("refresh").disabled = saving || loading || !selected;
  element("page-number").textContent = `Page ${pageIndex + 1}`;
  element("row-count").textContent = loading ? "Loading rows…" : `${rows.length} ${rows.length === 1 ? "row" : "rows"} on this page`;
  selectionControls();
}
async function load() {
  if (!selected) return;
  const version = ++generation;
  loading = true; dataLoaded = false; nextCursor = null; rows = []; selection.clear(); lastSelectionIndex = undefined; controls(); renderRows(); message();
  try {
    const page = await api<StudioPage>("rows", { table: selected.name, filters, ...(sort ? { sort } : {}), cursor: cursors[pageIndex], limit: Number(element<HTMLSelectElement>("page-size").value) });
    if (version !== generation) return;
    rows = page.rows; nextCursor = page.cursor; dataLoaded = true;
    element("connection-status").textContent = "Connected · PostgreSQL";
    element("database-state").textContent = "Connected";
  } catch (error) {
    if (version !== generation) return;
    message(error); element("connection-status").textContent = "Query failed · refresh to retry";
    element("database-state").textContent = "Query failed";
  } finally {
    if (version === generation) { loading = false; renderRows(); controls(); }
  }
}
function renderRows() {
  const head = document.querySelector<HTMLTableSectionElement>("#grid thead")!;
  const body = document.querySelector<HTMLTableSectionElement>("#grid tbody")!;
  const header = node("tr"), selectHeader = node("th", undefined, "selection-cell"), all = node("input");
  all.type = "checkbox"; all.id = "select-page"; all.setAttribute("aria-label", "Select all rows on this page");
  all.addEventListener("change", () => { selection.clear(); if (all.checked) for (const row of rows) selection.set(rowIdentity(row), row); selectionControls(); });
  selectHeader.append(all); header.append(selectHeader, node("th", "#", "row-number"));
  for (const field of selected?.fields ?? []) {
    const th = node("th");
    const label = node(field.scalar === "json" ? "span" : "button", undefined, "column-heading");
    const symbol = node("span", typeSymbol(field.scalar), typeClass(field.scalar));
    symbol.setAttribute("aria-hidden", "true");
    label.append(symbol, node("span", `${field.name}${sort?.field === field.name ? sort.direction === "asc" ? " ↑" : " ↓" : ""}`));
    if (field.name === selected?.primaryKey) { const key = icon("key"); key.setAttribute("class", "icon column-key"); label.append(key); }
    if (label instanceof HTMLButtonElement) {
      th.setAttribute("aria-sort", sort?.field === field.name ? sort.direction === "asc" ? "ascending" : "descending" : "none");
      label.title = `Sort by ${field.name}`;
      label.addEventListener("click", () => { sort = { field: field.name, direction: sort?.field === field.name && sort.direction === "asc" ? "desc" : "asc" }; reset(); });
    }
    th.append(label); header.append(th);
  }
  head.replaceChildren(header);
  body.replaceChildren(...rows.map((row, index) => {
    const tr = node("tr");
    const cell = node("td", undefined, "selection-cell"), check = node("input");
    check.type = "checkbox"; check.dataset.rowKey = rowIdentity(row); check.setAttribute("aria-label", `Select row ${display(row.values[selected!.primaryKey])}`);
    check.addEventListener("click", event => {
      event.stopPropagation();
      const from = event.shiftKey && lastSelectionIndex !== undefined ? Math.min(index, lastSelectionIndex) : index;
      const to = event.shiftKey && lastSelectionIndex !== undefined ? Math.max(index, lastSelectionIndex) : index;
      for (let i = from; i <= to; i++) { const value = rows[i]!; if (check.checked) selection.set(rowIdentity(value), value); else selection.delete(rowIdentity(value)); }
      lastSelectionIndex = index; selectionControls();
    });
    cell.addEventListener("click", event => event.stopPropagation()); cell.append(check); tr.append(cell);
    tr.append(node("td", String(pageIndex * Number(element<HTMLSelectElement>("page-size").value) + index + 1).padStart(2, "0"), "row-number"));
    for (const field of selected!.fields) {
      const value = row.values[field.name];
      const td = node("td"); td.title = display(value);
      if (field.name === selected!.primaryKey) {
        const open = node("button", display(value), "open-row");
        open.setAttribute("aria-label", `Inspect row ${display(value)}`);
        td.append(open);
      } else if (value === null) td.append(node("span", "null", "null"));
      else if (typeof value === "boolean") td.append(node("span", String(value), `bool ${value}`));
      else td.textContent = display(value);
      tr.append(td);
    }
    tr.addEventListener("click", () => inspect(row));
    return tr;
  }));
  const empty = element("empty"); empty.hidden = rows.length > 0;
  empty.querySelector("h2")!.textContent = loading ? "Reading your table…" : "No rows to show.";
  empty.querySelector("p")!.textContent = loading ? "Fetching a bounded page from the database." : filters.length ? "Try changing or removing a filter." : "This table is empty, or the last request could not complete.";
  selectionControls();
}
function rowIdentity(row: StudioRow): string { return JSON.stringify(row.values[selected!.primaryKey]); }
function selectionControls() {
  const count = selection.size;
  element("selection-bar").hidden = count === 0;
  element("selection-count").textContent = `${count} ${count === 1 ? "row" : "rows"} selected on this page`;
  const all = document.getElementById("select-page") as HTMLInputElement | null;
  if (all) { all.checked = rows.length > 0 && count === rows.length; all.indeterminate = count > 0 && count < rows.length; all.disabled = loading || saving || rows.length === 0; }
  for (const check of document.querySelectorAll<HTMLInputElement>("#grid input[data-row-key]")) {
    check.checked = selection.has(check.dataset.rowKey!); check.disabled = saving;
    check.closest("tr")!.dataset.selected = String(check.checked);
  }
  element<HTMLButtonElement>("delete-selected").hidden = schema?.readOnly ?? true;
}
element("clear-selection").addEventListener("click", () => { selection.clear(); lastSelectionIndex = undefined; selectionControls(); });
element("export-selected").addEventListener("click", () => {
  if (!selected || !selection.size) return;
  const file = new Blob([JSON.stringify({ table: selected.name, rows: [...selection.values()].map(row => row.values) }, null, 2)], { type: "application/json" });
  const url = URL.createObjectURL(file), link = node("a"); link.href = url;
  link.download = `${selected.name.replace(/[^a-zA-Z0-9_-]/g, "_").slice(0, 80)}-selected.json`;
  link.click(); setTimeout(() => URL.revokeObjectURL(url), 1000);
});
element("delete-selected").addEventListener("click", () => {
  if (!selected || !selection.size || saving || schema.readOnly) return;
  pendingDeletion = { table: selected, rows: [...selection.values()] };
  element("delete-title").textContent = `Delete ${selection.size} selected ${selection.size === 1 ? "row" : "rows"}?`;
  element("delete-description").textContent = `The selected rows will be permanently removed from “${selected.name}”. This cannot be undone in Studio.`;
  const references = relationships(schema.tables).filter(relation => relation.target.name === selected!.name);
  const effects = element("delete-effects"); effects.replaceChildren();
  if (references.length) {
    effects.append(node("strong", "Related table rules"));
    const list = node("ul");
    for (const relation of references) list.append(node("li", `${relation.source.name}.${relation.field.name} → ${relation.field.onDelete ?? "restrict"}`));
    effects.append(list, node("p", "Cascade rules can also delete related rows; restrict rules can prevent deletion."));
  }
  element("delete-error").hidden = true;
  element("confirm-delete").textContent = `Delete ${selection.size} ${selection.size === 1 ? "row" : "rows"}`;
  deleteDialog.showModal();
});
element("cancel-delete").addEventListener("click", () => { if (!saving) deleteDialog.close(); });
deleteDialog.addEventListener("cancel", event => { if (saving) event.preventDefault(); });
deleteDialog.addEventListener("close", () => { pendingDeletion = undefined; });
element<HTMLFormElement>("delete-form").addEventListener("submit", event => {
  event.preventDefault();
  if (!pendingDeletion || saving || schema.readOnly) return;
  const target = pendingDeletion;
  void (async () => {
    saving = true; element("delete-error").hidden = true;
    element<HTMLButtonElement>("confirm-delete").disabled = true; element<HTMLButtonElement>("cancel-delete").disabled = true;
    element("confirm-delete").textContent = "Deleting…";
    try {
      const result = await api<StudioDeleteResult>("delete", { table: target.table.name, rows: target.rows.map(row => ({ key: row.values[target.table.primaryKey], snapshot: row.snapshot })) });
      deleteDialog.close(); reset();
      element("edit-hint").textContent = `Deleted ${result.deleted} selected ${result.deleted === 1 ? "row" : "rows"} from ${target.table.name}.`;
    } catch (error) {
      element("delete-error").hidden = false; element("delete-error").textContent = error instanceof Error ? error.message : "Could not delete the selected rows";
    } finally {
      saving = false; element<HTMLButtonElement>("confirm-delete").disabled = false; element<HTMLButtonElement>("cancel-delete").disabled = false;
      element("confirm-delete").textContent = `Delete ${target.rows.length} ${target.rows.length === 1 ? "row" : "rows"}`;
      controls();
    }
  })();
});
function renderSchema(table: StudioTable) {
  const card = node("div", undefined, "schema-card"), title = node("h2");
  title.append(icon("table"), document.createTextNode("Fields & relationships")); card.append(title);
  const grid = node("table"), head = node("thead"), header = node("tr");
  for (const label of ["Field", "Type", "Nullable", "Default / generation", "Reference"]) header.append(node("th", label));
  head.append(header); grid.append(head);
  const body = node("tbody");
  for (const field of table.fields) {
    const tr = node("tr");
    for (const value of [field.name + (field.name === table.primaryKey ? " · primary key" : ""), field.scalar + (field.precision === undefined ? "" : ` (${field.precision})`), field.nullable ? "yes" : "no", field.hasDefault ? display(field.default) : field.name === table.primaryKey ? display(table.keyGeneration) : "—", field.reference ? `${field.reference} · ${field.onDelete ?? "restrict"}` : "—"]) tr.append(node("td", value));
    body.append(tr);
  }
  grid.append(body); card.append(grid);
  const detail = node("div", undefined, "schema-card"), detailTitle = node("h2");
  detailTitle.append(icon("code"), document.createTextNode("Constraints & indexes")); detail.append(detailTitle);
  detailTitle.append(node("span", "JSON", "code-language"));
  const pre = node("pre"), code = node("code", pretty({ indexes: table.indexes, unique: table.unique, checks: table.checks, fields: Object.fromEntries(table.fields.filter(field => field.constraints || field.jsonShape).map(field => [field.name, { constraints: field.constraints, jsonShape: field.jsonShape }])) }), "language-json");
  pre.append(code); detail.append(pre);
  element("schema-view").replaceChildren(card, detail);
  if (!element("schema-view").hidden) void highlightSchema(element("schema-view"));
}
function inspect(row: StudioRow) {
  if (!selected || saving) return;
  inspected = row; editors = [];
  element("editor-table").textContent = selected.name;
  element("editor-title").textContent = schema.readOnly ? "Inspect row" : "Edit row";
  element("editor-note").textContent = schema.readOnly ? "Read-only connection. Values are shown in their database representation." : "Changes are validated and saved in one managed transaction. Values use their database representation.";
  element("editor-error").hidden = true;
  element("cancel-editor").textContent = schema.readOnly ? "Close" : "Cancel";
  element<HTMLButtonElement>("save-row").hidden = schema.readOnly;
  element("editor-fields").replaceChildren(...selected.fields.map((field, index) => {
    const value = row.values[field.name], disabled = schema.readOnly || field.name === selected!.primaryKey;
    const wrapper = node("div", undefined, "editor-field");
    const label = node("label", undefined, "field-label"); label.htmlFor = `edit-${index}`;
    const symbol = node("span", typeSymbol(field.scalar), typeClass(field.scalar)); symbol.setAttribute("aria-hidden", "true");
    label.append(symbol, node("span", field.name), node("small", field.name === selected!.primaryKey ? `${field.scalar} · primary key` : field.scalar));
    let input: HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement;
    if (field.scalar === "json") { input = node("textarea"); input.value = value === null ? "" : pretty(value); }
    else if (field.scalar === "boolean") {
      input = node("select");
      for (const bool of ["false", "true"]) { const option = node("option", bool); option.value = bool; input.append(option); }
      input.value = String(value ?? false);
    } else { input = node("input"); input.type = "text"; input.value = value === null ? "" : display(value); }
    input.id = `edit-${index}`; input.disabled = disabled || value === null;
    input.autocomplete = "off";
    wrapper.append(label, input);
    let nullable: HTMLInputElement | undefined;
    if (field.nullable) {
      nullable = node("input"); nullable.type = "checkbox"; nullable.checked = value === null; nullable.disabled = disabled;
      const toggle = node("label", undefined, "null-toggle"); toggle.append(nullable, document.createTextNode("Set to null"));
      nullable.addEventListener("change", () => { input.disabled = disabled || nullable!.checked; });
      wrapper.append(toggle);
    }
    editors.push({ field, input, ...(nullable ? { nullable } : {}) });
    return wrapper;
  }));
  if (!dialog.open) dialog.showModal();
}
element<HTMLFormElement>("editor-form").addEventListener("submit", event => {
  event.preventDefault();
  if (saving || !selected || !inspected || schema.readOnly) return;
  void (async () => {
    const errorBox = element("editor-error"); errorBox.hidden = true;
    try {
      const patch = Object.fromEntries(editors.filter(({ field }) => field.name !== selected!.primaryKey).map(({ field, input, nullable }) => [field.name, nullable?.checked ? null : parsed(field, input.value)]).filter(([name, value]) => JSON.stringify(value) !== JSON.stringify(inspected!.values[name as string])));
      if (!Object.keys(patch).length) { dialog.close(); return; }
      saving = true; element<HTMLButtonElement>("save-row").disabled = true;
      element<HTMLButtonElement>("close-editor").disabled = true; element<HTMLButtonElement>("cancel-editor").disabled = true;
      await api<StudioRow>("update", { table: selected!.name, key: inspected!.values[selected!.primaryKey], snapshot: inspected!.snapshot, patch });
      dialog.close(); reset();
    } catch (error) {
      errorBox.hidden = false; errorBox.textContent = error instanceof Error ? error.message : "Could not save row";
    } finally {
      saving = false; element<HTMLButtonElement>("save-row").disabled = false;
      element<HTMLButtonElement>("close-editor").disabled = false; element<HTMLButtonElement>("cancel-editor").disabled = false;
    }
  })();
});
dialog.addEventListener("cancel", event => { if (saving) event.preventDefault(); });
for (const id of ["close-editor", "cancel-editor"]) element(id).addEventListener("click", () => dialog.close());
element("table-search").addEventListener("input", () => { if (schema) navigation(); });
element("filter-field").addEventListener("change", filterOperators);
element("filter-operator").addEventListener("change", () => { element<HTMLInputElement>("filter-value").disabled = ["is", "is not"].includes(element<HTMLSelectElement>("filter-operator").value); });
element<HTMLFormElement>("filter-form").addEventListener("submit", event => {
  event.preventDefault(); if (!selected) return;
  try {
    if (filters.length >= 12) throw new Error("Use at most 12 filters");
    const field = selected.fields.find(field => field.name === element<HTMLSelectElement>("filter-field").value)!;
    const operator = element<HTMLSelectElement>("filter-operator").value;
    const text = element<HTMLInputElement>("filter-value").value;
    filters.push({ field: field.name, operator, value: ["is", "is not"].includes(operator) ? null : operator === "contains" ? text : parsed(field, text) });
    reset();
  } catch (error) { message(error); }
});
element("refresh").addEventListener("click", reset);
element("page-size").addEventListener("change", reset);
element("previous").addEventListener("click", () => { if (!loading && pageIndex > 0) { pageIndex--; void load(); } });
element("next").addEventListener("click", () => { if (!loading && nextCursor) { cursors[++pageIndex] = nextCursor; void load(); } });
function showTab(tab: string) {
  for (const name of ["data", "schema"]) {
    const button = element<HTMLButtonElement>(`${name}-tab`);
    button.setAttribute("aria-selected", String(name === tab)); button.tabIndex = name === tab ? 0 : -1;
    element(`${name}-view`).hidden = name !== tab;
  }
  element("view-description").textContent = tab === "data" ? "Table contents" : "Fields, relationships & constraints";
  if (selected && pageView === "table") history.replaceState(null, "", `#table=${encodeURIComponent(selected.name)}&view=${tab}`);
  if (tab === "data" && selected && !dataLoaded && !loading) void load();
  if (tab === "schema") void highlightSchema(element("schema-view"));
}
for (const tab of ["data", "schema"]) {
  const button = element(`${tab}-tab`);
  button.addEventListener("click", () => showTab(tab));
  button.addEventListener("keydown", event => {
    if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
    event.preventDefault();
    const next = event.key === "Home" ? "data" : event.key === "End" ? "schema" : tab === "data" ? "schema" : "data";
    showTab(next); element(`${next}-tab`).focus();
  });
}
function themeLabel() {
  element("theme-toggle").setAttribute("aria-label", `Switch to ${document.documentElement.dataset.theme === "dark" ? "light" : "dark"} theme`);
}
themeLabel();
element("nav-map").addEventListener("click", () => { if (schema) workspace("map"); });
element("nav-catalog").addEventListener("click", () => { if (schema) workspace("catalog"); });
document.querySelector<HTMLAnchorElement>(".brand")!.addEventListener("click", event => { if (schema) { event.preventDefault(); workspace("map"); } });
window.addEventListener("popstate", () => { if (schema && !saving) route(); });
element("theme-toggle").addEventListener("click", () => {
  const theme = document.documentElement.dataset.theme === "dark" ? "light" : "dark";
  document.documentElement.dataset.theme = theme;
  try { localStorage.setItem("hulla-studio-theme", theme); } catch { /* Theme still works when storage is disabled. */ }
  themeLabel();
});
void (async () => {
  try {
    schema = await api<StudioSchema>("schema");
    element("namespace").textContent = schema.namespace;
    element("table-count").textContent = String(schema.tables.length).padStart(2, "0");
    element("access").replaceChildren(icon(schema.readOnly ? "lock" : "check"), document.createTextNode(schema.readOnly ? "Read only" : "Read / write"));
    element("database-state").textContent = "Schema loaded";
    element("connection-status").textContent = "Schema loaded · open a table to read data";
    const stats = [["Tables", schema.tables.length], ["References", relationships(schema.tables).length], ["Indexes", schema.tables.reduce((sum, table) => sum + indexCount(table), 0)], ["Unique / check rules", schema.tables.reduce((sum, table) => sum + ruleCount(table), 0)]] as const;
    element("schema-stats").replaceChildren(...stats.map(([label, count]) => { const card = node("div", undefined, "schema-stat"); card.append(node("span", label), node("strong", String(count).padStart(2, "0"))); return card; }));
    diagram = createDiagram(schema.tables, table => choose(table)); element("schema-diagram").append(diagram.root);
    element("schema-catalog").append(createCatalog(schema.tables, choose));
    route();
  } catch (error) { message(error); element("access").textContent = "Disconnected"; element("connection-status").textContent = "Connection unavailable"; }
})();
