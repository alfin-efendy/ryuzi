# Task 2 Report

- Renamed the Cockpit Apps hub view/tests to `PluginsView`, switched the top-level nav route from `apps` to `plugins`, and removed the standalone registry route from `App.tsx`.
- Updated the sidebar main nav label/grouping to `Plugins`, renamed the plugin-only sidebar section to `Enabled plugins`, and kept installed MCP server detail on `appDetail`.
- Replaced the old registry navigation button in the renamed view with an in-place `Browse plugins` action that switches to the existing catalog tab.
- Updated app/registry back-navigation copy to `Plugins` so the focused Cockpit build stays type-safe after the route rename.

Verification:

- `bun test apps/cockpit/src/views/PluginsView.test.tsx`
- `bun run --cwd apps/cockpit build`

Follow-up (Task 2 review finding):

- Fixed `apps/cockpit/src/views/RuntimeDetailView.tsx` copy to replace the stale
  user-facing phrase with `Plugins`:  
  `No plugins installed yet — add MCP servers from the Plugins screen.`
- Did a narrow scan for remaining direct `Apps screen` / `Apps` nav copy fallout in
  Cockpit views; found only this runtime-detail string plus a test fixture copy in
  `RuntimeDetailView.test.tsx` and an unrelated confirmation copy in
  `ModelsView.tsx` (`Apps using it`), which were not part of the Apps-navigation
  rename.

- Updated `apps/cockpit/src/views/RuntimeDetailView.test.tsx` to match new copy
  (`No plugins installed yet — add MCP servers from the Plugins screen.`) and ran:
  `bun test apps/cockpit/src/views/RuntimeDetailView.test.tsx` (pass: 7/7 tests).
