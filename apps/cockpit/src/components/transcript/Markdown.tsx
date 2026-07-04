import { Component, memo, type ReactNode } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import rehypeHighlight from "rehype-highlight";
import { openUrl } from "@tauri-apps/plugin-opener";

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
            a: ({ node: _node, href, children, ...rest }) => (
              <a
                {...rest}
                href={href}
                onClick={(e) => {
                  e.preventDefault();
                  if (href) void openUrl(href);
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
