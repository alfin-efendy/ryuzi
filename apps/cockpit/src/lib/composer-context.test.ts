import { expect, test } from "bun:test";
import { activeContextQuery, replaceActiveContextToken } from "./composer-context";

test("detects only path-shaped @ context queries at the end of the draft", () => {
  expect(activeContextQuery("review @src/views")).toEqual({ start: 7, query: "src/views" });
  expect(activeContextQuery("review @README.md")).toEqual({ start: 7, query: "README.md" });
  expect(activeContextQuery("@a")).toBeNull();
  expect(activeContextQuery("email me@work")).toBeNull();
  expect(activeContextQuery("review @src then")).toBeNull();
});

test("replaces the active @ token with the selected project path", () => {
  expect(replaceActiveContextToken("review @src/vi", "src/views/HomeView.tsx")).toBe("review @src/views/HomeView.tsx ");
  expect(replaceActiveContextToken("@", "README.md")).toBe("@README.md ");
});
