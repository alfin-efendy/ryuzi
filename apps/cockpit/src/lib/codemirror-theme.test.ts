import { expect, test } from "bun:test";
import { tags } from "@lezer/highlight";
import { cockpitCodeTheme, cockpitHighlightStyle } from "./codemirror-theme";

test("cockpitCodeTheme bundles the editor theme and syntax highlighting", () => {
  expect(Array.isArray(cockpitCodeTheme)).toBe(true);
  expect((cockpitCodeTheme as unknown as unknown[]).length).toBe(2);
});

test("highlight style resolves a class for every mirrored .chat-md group", () => {
  for (const tag of [tags.keyword, tags.string, tags.number, tags.comment, tags.attributeName, tags.className]) {
    expect(cockpitHighlightStyle.style([tag])).toBeTruthy();
  }
});

test("keyword and string map to distinct classes", () => {
  expect(cockpitHighlightStyle.style([tags.keyword])).not.toBe(cockpitHighlightStyle.style([tags.string]));
});
