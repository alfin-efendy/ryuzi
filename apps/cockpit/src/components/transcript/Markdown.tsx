import { Component, createContext, memo, useContext, type ComponentProps, type ReactNode } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import rehypeHighlight from "rehype-highlight";
import { openUrl } from "@tauri-apps/plugin-opener";
import { parsePathToken } from "@/lib/paths";

/** A renderer crash must never lose content: fall back to plain pre-wrap text. */
class Boundary extends Component<{ raw: string; children: ReactNode }, { failed: boolean }> {
  state = { failed: false };
  static getDerivedStateFromError() {
    return { failed: true };
  }
  render() {
    if (this.state.failed) return <div className="whitespace-pre-wrap">{this.props.raw}</div>;
    return this.props.children;
  }
}

/** Session-scoped handler that opens a workdir-relative file path in the
 *  right panel. Null outside a session: paths render as plain code. */
export const FileOpenContext = createContext<((path: string) => void) | null>(null);

/** Inline-code renderer: a path-like token becomes clickable when a handler
 *  is in context. Block code (fenced → `language-*` class or multi-line /
 *  non-string children from rehype-highlight) always renders plain. */
function CodeToken({ className, children, ...rest }: ComponentProps<"code">) {
  const openPath = useContext(FileOpenContext);
  const text =
    typeof children === "string"
      ? children
      : Array.isArray(children) && children.every((c) => typeof c === "string")
        ? children.join("")
        : null;
  const isBlock = (className ?? "").includes("language-") || text === null || text.includes("\n");
  const parsed = !isBlock && openPath !== null && text !== null ? parsePathToken(text) : null;
  if (parsed === null || openPath === null) {
    return (
      <code className={className} {...rest}>
        {children}
      </code>
    );
  }
  return (
    <code
      role="button"
      tabIndex={0}
      title={`Open ${parsed.path}`}
      className={`${className ?? ""} cursor-pointer underline decoration-dotted underline-offset-2 hover:text-primary`}
      onClick={() => openPath(parsed.path)}
      onKeyDown={(e) => {
        if (e.key === "Enter") openPath(parsed.path);
      }}
      {...rest}
    >
      {children}
    </code>
  );
}

// AST-only markdown for agent output. No rehype-raw, ever: the webview runs
// with csp:null, so raw HTML must stay inert (react-markdown's default).
// Links open in the system browser — in-webview navigation would leave the app.
export const Markdown = memo(function Markdown({ text }: { text: string }) {
  return (
    <Boundary raw={text}>
      <div className="chat-md">
        <ReactMarkdown
          remarkPlugins={[remarkGfm]}
          rehypePlugins={[rehypeHighlight]}
          components={{
            code: CodeToken,
            a: ({ node: _node, href, children, ...rest }) => (
              <a
                {...rest}
                href={href}
                onClick={(e) => {
                  e.preventDefault();
                  if (href && /^https?:/i.test(href)) {
                    openUrl(href).catch((err) => console.warn("openUrl failed", err));
                  }
                }}
              >
                {children}
              </a>
            ),
          }}
        >
          {text}
        </ReactMarkdown>
      </div>
    </Boundary>
  );
});
