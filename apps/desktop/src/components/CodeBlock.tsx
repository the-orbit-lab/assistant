/**
 * A fenced code block with copy.
 *
 * No syntax highlighting: it would pull a large grammar bundle into an
 * app whose code blocks are mostly short Rust and shell excerpts, and
 * the highlighting cost lands on every streamed token. The language tag
 * is shown instead, which is the part that actually helps a reader.
 */

import { memo, useCallback, useState } from "react";

interface Props {
  code: string;
  language?: string;
}

export const CodeBlock = memo(function CodeBlock({ code, language }: Props) {
  const [copied, setCopied] = useState(false);

  const copy = useCallback(() => {
    void navigator.clipboard.writeText(code).then(() => {
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1400);
    });
  }, [code]);

  return (
    <div className="code-block">
      <div className="code-head">
        <span className="lang">{language || "text"}</span>
        <button className="copy" onClick={copy} aria-label="Copy code">
          {copied ? "Copied" : "Copy"}
        </button>
      </div>
      <pre>
        <code>{code}</code>
      </pre>
    </div>
  );
});

export const InlineCode = memo(function InlineCode({ children }: { children: React.ReactNode }) {
  return <code className="inline-code">{children}</code>;
});
