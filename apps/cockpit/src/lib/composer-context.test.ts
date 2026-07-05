import { expect, test } from "bun:test";
import { activeContextQuery, replaceActiveContextToken } from "./composer-context";

test("detects the active @ context query at the end of the draft", () => {
  expect(activeContextQuery("review @src/views")).toEqual({ start: 7, query: "src/views" });
  expect(activeContextQuery("@")).toEqual({ start: 0, query: "" });
  expect(activeContextQuery("email me@work")).toBeNull();
  expect(activeContextQuery("review @src then")).toBeNull();
});

test("replaces the active @ token with the selected project path", () => {
  expect(replaceActiveContextToken("review @src/vi", "src/views/HomeView.tsx")).toBe("review @src/views/HomeView.tsx ");
  expect(replaceActiveContextToken("@", "README.md")).toBe("@README.md ");
});
