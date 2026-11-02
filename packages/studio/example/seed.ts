import { db } from "./_generated/server.ts";
try {
  await db.transaction(async tx => {
    if (await tx.select("projects").where({ slug: "studio" }).unique()) {
      console.log("Example data already exists; preserving your edits."); return;
    }
    await tx.insert("projects", { slug: "studio", name: "Hulla Studio", description: "A local workbench for your database." });
    const titles = ["Explore the schema", "Try filtering this table", "Edit a task title", "Inspect a JSON field", "Sort by priority", "Browse the next page", "Check read-only mode", "Review relationships"];
    await tx.insert("tasks", Array.from({ length: 64 }, (_, index) => ({
      project: "studio", title: `${titles[index % titles.length]}${index >= titles.length ? ` · ${index + 1}` : ""}`,
      status: ["backlog", "in_progress", "done"][index % 3]!, priority: index % 6,
      completed: index % 3 === 2, due: index % 4 === 0 ? null : "2026-11-01",
      notes: index % 5 === 0 ? "Open this row to edit its values." : null,
      labels: { tags: index % 2 === 0 ? ["studio", "local"] : ["database"] },
    })));
    console.log("Added one project and 64 tasks.");
  });
} finally { await db.close(); }
