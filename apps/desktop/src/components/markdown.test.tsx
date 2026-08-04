import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { MarkdownMessage } from "./MarkdownMessage";

const html = (markdown: string) => render(<MarkdownMessage text={markdown} />).container;

describe("MarkdownMessage", () => {
  it("renders headings as headings, not as hashes", () => {
    expect(html("# Claude Code Instructions").querySelector("h1")?.textContent).toBe(
      "Claude Code Instructions",
    );
  });

  it("renders bold text without showing the asterisks", () => {
    const container = html("**CLI Commands**");
    expect(container.querySelector("strong")?.textContent).toBe("CLI Commands");
    expect(container.textContent).not.toContain("**");
  });

  it("renders inline code without showing the backticks", () => {
    const container = html("Read `CLAUDE.md` first.");
    expect(container.querySelector("code.inline-code")?.textContent).toBe("CLAUDE.md");
    expect(container.textContent).not.toContain("`");
  });

  it("renders bullet lists as list items", () => {
    const container = html("- first\n- second\n- third");
    expect(container.querySelectorAll("li")).toHaveLength(3);
  });

  it("renders ordered lists", () => {
    const container = html("1. one\n2. two");
    expect(container.querySelector("ol")).toBeTruthy();
  });

  it("renders a fenced code block with a copy button", () => {
    const container = html("```rust\nfn main() {}\n```");
    expect(container.querySelector(".code-block code")?.textContent).toBe("fn main() {}");
    expect(screen.getByLabelText("Copy code")).toBeTruthy();
    expect(container.querySelector(".lang")?.textContent).toBe("rust");
  });

  it("renders GFM tables", () => {
    const container = html("| a | b |\n| --- | --- |\n| 1 | 2 |");
    expect(container.querySelectorAll("th")).toHaveLength(2);
    expect(container.querySelectorAll("td")).toHaveLength(2);
  });

  it("renders blockquotes and horizontal rules", () => {
    expect(html("> quoted").querySelector("blockquote")).toBeTruthy();
    expect(html("---").querySelector("hr")).toBeTruthy();
  });

  /// Answers are generated text quoting repository content. Treating any
  /// of it as markup would hand a model a DOM.
  it("never renders raw HTML", () => {
    const container = html('Before <img src=x onerror="alert(1)"> after');
    expect(container.querySelector("img")).toBeNull();
    expect(container.textContent).toContain("Before");
  });

  it("does not execute a script tag", () => {
    const container = html("<script>window.pwned = 1</script>");
    expect(container.querySelector("script")).toBeNull();
  });

  it("opens http links safely in a new tab", () => {
    const link = html("[docs](https://example.com/x)").querySelector("a")!;
    expect(link.getAttribute("target")).toBe("_blank");
    expect(link.getAttribute("rel")).toContain("noopener");
  });

  /// A javascript: or file: target is shown as text, never as a link.
  it("refuses a non-http link scheme", () => {
    const container = html("[click](javascript:alert(1))");
    expect(container.querySelector("a")).toBeNull();
    expect(container.querySelector(".link-blocked")?.textContent).toBe("click");
  });

  it("renders partial markdown while it is still streaming", () => {
    // An unterminated fence is what a half-arrived answer looks like.
    const container = html("Here is the code:\n```rust\nfn main() {");
    expect(container.textContent).toContain("Here is the code:");
    expect(container.querySelector(".code-block")).toBeTruthy();
  });
});
