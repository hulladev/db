import type { StudioField, StudioTable } from "../types.ts";

export interface Relation { source: StudioTable; field: StudioField; target: StudioTable }
export interface DiagramNode {
  table: StudioTable; fields: readonly StudioField[]; omitted: number;
  x: number; y: number; width: number; height: number;
}
export interface DiagramEdge extends Relation { path: string }
export interface DiagramLayout { nodes: DiagramNode[]; edges: DiagramEdge[]; width: number; height: number }
export function relationships(tables: readonly StudioTable[]): Relation[] {
  const byName = new Map(tables.map(table => [table.name, table]));
  return tables.flatMap(source => source.fields.flatMap(field => {
    const target = field.reference === undefined ? undefined : byName.get(field.reference);
    return target ? [{ source, field, target }] : [];
  }));
}
export const indexCount = (table: StudioTable) => Array.isArray(table.indexes) ? table.indexes.length : 0;
export const ruleCount = (table: StudioTable) => (Array.isArray(table.checks) ? table.checks.length : 0)
  + (table.unique && typeof table.unique === "object" ? Object.keys(table.unique).length : 0);

/** Lay out connected components in columns. Iterative traversal handles cycles
 * and self references without recursive depth or duplicate table cards. */
export function layoutSchema(tables: readonly StudioTable[]): DiagramLayout {
  const relations = relationships(tables), byName = new Map(tables.map(table => [table.name, table]));
  const neighbors = new Map(tables.map(table => [table.name, new Set<string>()]));
  for (const { source, target } of relations) {
    if (source !== target) { neighbors.get(source.name)!.add(target.name); neighbors.get(target.name)!.add(source.name); }
  }
  const rank = (a: string, b: string) => neighbors.get(b)!.size - neighbors.get(a)!.size || a.localeCompare(b);
  const pending = new Set(tables.map(table => table.name)), nodes: DiagramNode[] = [];
  let top = 50, width = 500;
  for (const root of [...pending].sort(rank)) {
    if (!pending.delete(root)) continue;
    const levels = new Map<string, number>([[root, 0]]), queue = [root];
    for (let cursor = 0; cursor < queue.length; cursor++) {
      const name = queue[cursor]!;
      for (const neighbor of [...neighbors.get(name)!].sort()) {
        if (!pending.delete(neighbor)) continue;
        levels.set(neighbor, levels.get(name)! + 1); queue.push(neighbor);
      }
    }
    const columns = new Map<number, DiagramNode[]>();
    for (const [name, level] of levels) {
      const table = byName.get(name)!;
      const primary = table.fields.filter(field => field.name === table.primaryKey);
      const foreign = table.fields.filter(field => field.name !== table.primaryKey && field.reference);
      const ordinary = table.fields.filter(field => field.name !== table.primaryKey && !field.reference).slice(0, 6);
      const fields = [...primary, ...foreign, ...ordinary];
      const column = columns.get(level) ?? []; columns.set(level, column);
      column.push({ table, fields, omitted: table.fields.length - fields.length, x: 50 + level * 450, y: 0, width: 286, height: 80 + fields.length * 25 });
    }
    const heights = [...columns.values()].map(column => column.reduce((sum, item) => sum + item.height + 45, -45));
    const height = Math.max(0, ...heights);
    let index = 0;
    for (const column of columns.values()) {
      let y = top + (height - heights[index++]!) / 2;
      for (const item of column) { item.y = y; y += item.height + 45; nodes.push(item); width = Math.max(width, item.x + item.width + 120); }
    }
    top += height + 90;
  }
  const placed = new Map(nodes.map(item => [item.table.name, item]));
  const edges = relations.map(relation => {
    const source = placed.get(relation.source.name)!, target = placed.get(relation.target.name)!;
    const sy = source.y + 50 + source.fields.findIndex(field => field.name === relation.field.name) * 25 + 12.5;
    const ty = target.y + 62.5; // Primary keys are the first visible field.
    let path: string;
    if (source.x === target.x) {
      const sx = source.x + source.width, tx = target.x + target.width, bend = Math.max(sx, tx) + 65;
      const loop = source === target && sy === ty ? 45 : 0;
      path = `M${sx},${sy} C${bend},${sy - loop} ${bend},${ty + loop} ${tx},${ty}`;
    } else {
      const left = source.x > target.x, sx = left ? source.x : source.x + source.width;
      const tx = left ? target.x + target.width : target.x, bend = Math.abs(tx - sx) / 2;
      path = `M${sx},${sy} C${sx + (left ? -bend : bend)},${sy} ${tx + (left ? bend : -bend)},${ty} ${tx},${ty}`;
    }
    return { ...relation, path };
  });
  return { nodes, edges, width, height: Math.max(320, top - 40) };
}
