// bun test preload: registers a DOM (window/document/etc.) so React component
// tests can render. Pure-logic tests are unaffected beyond the added globals.
import { GlobalRegistrator } from "@happy-dom/global-registrator";

GlobalRegistrator.register();
