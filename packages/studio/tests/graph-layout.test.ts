import { expect, test } from "bun:test";
import { layoutSchema } from "../src/ui/graph-layout.ts";
import type { StudioField, StudioTable } from "../src/types.ts";

const field = (name: string, reference?: string): StudioField => ({ name, scalar: "string", nullable: false, hasDefault: false, ...(reference ? { reference } : {}) });
const table = (name: string, fields: StudioField[]): StudioTable => ({ name, primaryKey: "id", keyGeneration: "none", fields: [field("id"), ...fields], indexes: [], unique: {}, checks: [] });

test("cyclic, self-referencing and disconnected schemas keep every table reachable without overlapping cards", () => {
  const input = [table("a", [field("parent", "b"), field("self", "a")]), table("b", [field("parent", "a")]), table("isolated", [])];
  const layout = layoutSchema(input);
  expect(layout.nodes.map(node => node.table.name).sort()).toEqual(["a", "b", "isolated"]);
  expect(layout.edges.map(edge => `${edge.source.name}.${edge.field.name}->${edge.target.name}`).sort()).toEqual(["a.parent->b", "a.self->a", "b.parent->a"]);
  for (let i = 0; i < layout.nodes.length; i++) {
    const first = layout.nodes[i]!;
    expect(first.x >= 0 && first.y >= 0 && first.x + first.width <= layout.width && first.y + first.height <= layout.height).toBe(true);
    for (const second of layout.nodes.slice(i + 1)) expect(first.x + first.width <= second.x || second.x + second.width <= first.x || first.y + first.height <= second.y || second.y + second.height <= first.y).toBe(true);
  }
});

test("compact cards retain foreign keys beyond the ordinary-field preview and attach edges to visible fields", () => {
  const source = table("source", [...Array.from({ length: 12 }, (_, index) => field(`text${index}`)), field("late_reference", "target")]);
  const layout = layoutSchema([source, table("target", [])]);
  const card = layout.nodes.find(node => node.table.name === "source")!;
  expect(card.fields.map(field => field.name)).toContain("late_reference");
  expect(card.fields[0]!.name).toBe("id");
  expect(card.omitted).toBeGreaterThan(0);
  expect(layout.edges[0]!.field.name).toBe("late_reference");
  expect(layout.edges[0]!.path).not.toContain("NaN");
});
