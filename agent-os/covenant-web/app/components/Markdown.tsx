import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

/** Renders the coding agent's markdown reply (code blocks, tables, lists).
 *  No raw HTML is rendered — react-markdown escapes it — so agent output is
 *  safe to display. */
export function Markdown({ children }: { children: string }) {
  return (
    <div className="md">
      <ReactMarkdown remarkPlugins={[remarkGfm]}>{children}</ReactMarkdown>
    </div>
  );
}
