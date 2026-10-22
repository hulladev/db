declare module "*.svelte" {
  import type { Component } from "svelte";
  const component: Component<import("./fixture-types.ts").FixtureProps>;
  export default component;
}

declare module "*PaginationFixture.svelte" {
  import type { Component } from "svelte";
  const component: Component<import("./fixture-types.ts").PaginationFixtureProps>;
  export default component;
}
