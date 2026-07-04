import { test, expect } from "bun:test";
import { languageFor } from "./language";

test("languageFor matches common filenames", () => {
  expect(languageFor("main.ts")?.name).toBe("TypeScript");
  expect(languageFor("lib.rs")?.name).toBe("Rust");
  expect(languageFor("README.md")?.name).toBe("Markdown");
});

test("languageFor returns null for unknown extensions", () => {
  expect(languageFor("data.xyzunknown")).toBeNull();
});
