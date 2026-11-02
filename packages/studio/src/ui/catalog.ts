import type { StudioTable } from "../types.ts";
import { indexCount, relationships, ruleCount } from "./graph-layout.ts";
import { buttonClass, icon } from "./style.ts";

function node<K extends keyof HTMLElementTagNameMap>(tag: K, className: string, text?: string): HTMLElementTagNameMap[K] {
  const value = document.createElement(tag); value.className = className;
  if (text !== undefined) value.textContent = text;
  return value;
}
export function createCatalog(tables: readonly StudioTable[], open: (table: StudioTable, tab?: "data" | "schema") => void): HTMLElement {
  const root = node("div", "catalog"), toolbar = node("div", "catalog-toolbar"), search = node("input", "catalog-search");
  search.type = "search"; search.placeholder = "Find a table, field, or type…"; search.setAttribute("aria-label", "Search schema catalog");
  const count = node("span", "catalog-count"); toolbar.append(search, count);
  const content = node("div", "catalog-content"); root.append(toolbar, content);
  const relations = relationships(tables);
  function render() {
    const query = search.value.toLowerCase().trim();
    const visible = tables.filter(table => [table.name, ...table.fields.flatMap(field => [field.name, field.scalar, field.reference ?? ""])].some(value => value.toLowerCase().includes(query)));
    count.textContent = `${visible.length} of ${tables.length} tables`;
    const grid = node("div", "catalog-grid");
    for (const table of visible) {
      const card = node("article", "catalog-card"), title = node("button", "catalog-title"); title.type = "button"; title.setAttribute("aria-label", `Browse ${table.name}`);
      title.append(icon("table"), node("strong", "", table.name)); title.addEventListener("click", () => open(table)); card.append(title);
      card.append(node("p", "catalog-meta", `${table.fields.length} fields · ${indexCount(table)} indexes · ${ruleCount(table)} unique/check rules`));
      const fields = node("div", "catalog-fields");
      for (const field of table.fields.slice(0, 8)) {
        const chip = node("span", "catalog-field", field.name); chip.title = `${field.scalar}${field.nullable ? " · nullable" : ""}${field.reference ? ` → ${field.reference}` : ""}`;
        if (field.name === table.primaryKey) chip.prepend(icon("key")); fields.append(chip);
      }
      if (table.fields.length > 8) fields.append(node("span", "catalog-more", `+${table.fields.length - 8}`)); card.append(fields);
      const action = node("button", buttonClass(), "Inspect schema →"); action.type = "button"; action.setAttribute("aria-label", `Inspect schema of ${table.name}`); action.addEventListener("click", () => open(table, "schema")); card.append(action); grid.append(card);
    }
    const relationSection = node("section", "catalog-relations"); relationSection.append(node("h2", "", "References & delete behavior"));
    const visibleNames = new Set(visible.map(table => table.name));
    const list = node("div", "reference-list");
    for (const relation of relations.filter(item => visibleNames.has(item.source.name) || visibleNames.has(item.target.name))) {
      const row = node("div", "reference-item"), source = node("button", "reference-link", `${relation.source.name}.${relation.field.name}`), target = node("button", "reference-link", `${relation.target.name}.${relation.target.primaryKey}`);
      source.addEventListener("click", () => open(relation.source, "schema")); target.addEventListener("click", () => open(relation.target, "schema"));
      row.append(source, icon("arrow"), target, node("span", "reference-policy", relation.field.onDelete ?? "restrict"));
      if (relation.field.nullable) row.append(node("span", "reference-nullable", "nullable")); list.append(row);
    }
    if (!list.children.length) list.append(node("p", "catalog-empty", query ? "No references match this search." : "No references are declared in this schema."));
    relationSection.append(list);
    content.replaceChildren(grid, relationSection);
    if (!visible.length) grid.append(node("p", "catalog-empty", "No tables or fields match this search."));
  }
  search.addEventListener("input", render); render(); return root;
}
