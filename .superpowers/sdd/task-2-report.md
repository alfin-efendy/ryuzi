# Task 2 Report

- Renamed the Cockpit Apps hub view/tests to `PluginsView`, switched the top-level nav route from `apps` to `plugins`, and removed the standalone registry route from `App.tsx`.
- Updated the sidebar main nav label/grouping to `Plugins`, renamed the plugin-only sidebar section to `Enabled plugins`, and kept installed MCP server detail on `appDetail`.
- Replaced the old registry navigation button in the renamed view with an in-place `Browse plugins` action that switches to the existing catalog tab.
- Updated app/registry back-navigation copy to `Plugins` so the focused Cockpit build stays type-safe after the route rename.

Verification:

- `bun test apps/cockpit/src/views/PluginsView.test.tsx`
- `bun run --cwd apps/cockpit build`
