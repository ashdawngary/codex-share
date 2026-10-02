import { memo, useEffect, useState, type ReactNode } from "react";
import ReactMarkdown, { type Components, defaultUrlTransform } from "react-markdown";
import remarkBreaks from "remark-breaks";
import remarkGfm from "remark-gfm";
import type { ThemedToken } from "@shikijs/core";

function safeUrlTransform(url: string): string {
  if (url.startsWith("#")) return url;
  try {
    const parsed = new URL(url);
    return parsed.protocol === "http:" || parsed.protocol === "https:"
      ? defaultUrlTransform(url)
      : "";
  } catch {
    return "";
  }
}

function CodeBlock({ code, language }: { code: string; language: string }) {
  const [tokens, setTokens] = useState<ThemedToken[][] | null>(null);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    let current = true;
    import("./highlight")
      .then(({ highlight }) => highlight(code, language))
      .then((result) => {
        if (current) setTokens(result);
      })
      .catch(() => {
        if (current) setTokens(null);
      });
    return () => {
      current = false;
    };
  }, [code, language]);

  async function copyCode() {
    try {
      await navigator.clipboard.writeText(code);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1400);
    } catch {
      setCopied(false);
    }
  }

  return (
    <div className="code-block">
      <div className="code-toolbar">
        <span>{language || "text"}</span>
        <button type="button" onClick={copyCode}>{copied ? "Copied" : "Copy"}</button>
      </div>
      <pre>
        <code>
          {tokens
            ? tokens.map((line, lineIndex) => (
                <span className="code-line" key={lineIndex}>
                  {line.map((token, tokenIndex) => (
                    <span key={tokenIndex} style={{ color: token.color }}>{token.content}</span>
                  ))}
                  {lineIndex < tokens.length - 1 ? "\n" : null}
                </span>
              ))
            : code}
        </code>
      </pre>
    </div>
  );
}

function plainText(children: ReactNode): string {
  return Array.isArray(children) ? children.join("") : String(children ?? "");
}

const components: Components = {
  a({ href, children }) {
    if (!href) return <span className="unsafe-link">{children}</span>;
    return <a href={href} target="_blank" rel="noopener noreferrer">{children}</a>;
  },
  img({ alt }) {
    return <span className="markdown-image-blocked">[image omitted{alt ? `: ${alt}` : ""}]</span>;
  },
  pre({ children }) {
    return <>{children}</>;
  },
  code({ className, children, node }) {
    const value = plainText(children).replace(/\n$/, "");
    const language = /language-([^\s]+)/.exec(className ?? "")?.[1] ?? "";
    const spansLines = node?.position?.start.line !== node?.position?.end.line;
    if (className || spansLines || value.includes("\n")) return <CodeBlock code={value} language={language} />;
    return <code className="inline-code">{children}</code>;
  },
};

export const Markdown = memo(function Markdown({ children }: { children: string }) {
  return (
    <div className="markdown">
      <ReactMarkdown
        remarkPlugins={[remarkGfm, remarkBreaks]}
        components={components}
        skipHtml
        urlTransform={safeUrlTransform}
      >
        {children}
      </ReactMarkdown>
    </div>
  );
});
