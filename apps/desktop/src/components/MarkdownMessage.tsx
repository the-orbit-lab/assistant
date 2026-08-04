/**
 * Assistant prose, rendered as Markdown.
 *
 * Two things shape this component.
 *
 * **Raw HTML is never rendered.** `react-markdown` disallows it by
 * default and no `rehype-raw` is configured. Orbit's answers are
 * generated text quoting repository content, so treating any of it as
 * markup would be handing a model a DOM. Adding HTML support later means
 * adding sanitization first.
 *
 * **Streaming stays cheap.** The component is memoized on its text, so a
 * `response_delta` re-parses only the message it belongs to and leaves
 * every earlier turn alone. Markdown that is still arriving parses fine
 * as partial input — an unterminated fence renders as a code block that
 * grows, which is what a reader expects to see.
 */

import { memo } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { CodeBlock, InlineCode } from "./CodeBlock";

/** Links open in the user's browser, never inside the app shell. */
function SafeLink({ href, children }: { href?: string; children?: React.ReactNode }) {
  const safe = href && /^https?:\/\//i.test(href);
  if (!safe) {
    // A non-http scheme (javascript:, file:, data:) is shown as text.
    return <span className="link-blocked">{children}</span>;
  }
  return (
    <a href={href} target="_blank" rel="noreferrer noopener">
      {children}
    </a>
  );
}

export const MarkdownMessage = memo(function MarkdownMessage({ text }: { text: string }) {
  return (
    <div className="markdown">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={{
          a: ({ href, children }) => <SafeLink href={href}>{children}</SafeLink>,
          code({ className, children, ...props }) {
            const text = String(children).replace(/\n$/, "");
            // react-markdown gives fenced blocks a `language-x` class and
            // inline code none, which is how the two are told apart.
            const fenced = /language-(\w+)/.exec(className || "");
            if (fenced || text.includes("\n")) {
              return <CodeBlock code={text} language={fenced?.[1]} />;
            }
            return <InlineCode {...props}>{children}</InlineCode>;
          },
          // The default wraps fenced blocks in <pre>, which would nest
          // inside CodeBlock's own <pre>.
          pre: ({ children }) => <>{children}</>,
        }}
      >
        {text}
      </ReactMarkdown>
    </div>
  );
});
