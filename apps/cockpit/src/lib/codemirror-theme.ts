import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import type { Extension } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { tags as t } from "@lezer/highlight";

// Editor chrome mapped to app tokens. CSS custom properties resolve live in
// the DOM, so one definition covers light and dark — no useTheme subscription
// or re-render is needed when the .dark class flips on <html>.
const editorTheme = EditorView.theme({
  "&": {
    backgroundColor: "var(--code)",
    color: "var(--code-foreground)",
    fontSize: "12px",
  },
  ".cm-content": { caretColor: "var(--foreground)" },
  ".cm-cursor, .cm-dropCursor": { borderLeftColor: "var(--foreground)" },
  ".cm-gutters": {
    backgroundColor: "var(--code)",
    color: "var(--code-number)",
    borderRight: "1px solid var(--border)",
  },
  ".cm-activeLine": { backgroundColor: "var(--code-highlight)" },
  ".cm-activeLineGutter": { backgroundColor: "var(--code-highlight)" },
  "&.cm-focused .cm-selectionBackground, .cm-selectionBackground, .cm-content ::selection": {
    background: "color-mix(in oklab, var(--primary) 20%, transparent)",
  },
});

// Syntax palette mirroring the .chat-md hljs mapping (apps/cockpit/src/index.css).
export const cockpitHighlightStyle = HighlightStyle.define([
  { tag: [t.comment, t.quote], color: "var(--syntax-comment)", fontStyle: "italic" },
  { tag: [t.keyword, t.self, t.modifier, t.typeName, t.tagName], color: "var(--syntax-keyword)" },
  { tag: [t.string, t.special(t.string), t.regexp, t.inserted], color: "var(--syntax-string)" },
  { tag: [t.number, t.bool, t.null, t.atom], color: "var(--syntax-number)" },
  { tag: [t.function(t.variableName), t.function(t.propertyName), t.className, t.heading], color: "var(--syntax-title)" },
  { tag: [t.attributeName, t.propertyName, t.variableName], color: "var(--syntax-attr)" },
  { tag: t.deleted, color: "var(--destructive)" },
  { tag: t.emphasis, fontStyle: "italic" },
  { tag: t.strong, fontWeight: "600" },
]);

/** Drop-in for react-codemirror's `theme` prop — replaces its built-in light theme. */
export const cockpitCodeTheme: Extension = [editorTheme, syntaxHighlighting(cockpitHighlightStyle)];
