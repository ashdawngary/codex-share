import { createHighlighterCore, type ThemedToken } from "@shikijs/core";
import { createOnigurumaEngine } from "@shikijs/engine-oniguruma";
import getWasmInstance from "@shikijs/engine-oniguruma/wasm-inlined";
import css from "@shikijs/langs/css";
import html from "@shikijs/langs/html";
import javascript from "@shikijs/langs/javascript";
import json from "@shikijs/langs/json";
import jsx from "@shikijs/langs/jsx";
import markdown from "@shikijs/langs/markdown";
import python from "@shikijs/langs/python";
import rust from "@shikijs/langs/rust";
import shellscript from "@shikijs/langs/shellscript";
import tsx from "@shikijs/langs/tsx";
import typescript from "@shikijs/langs/typescript";
import yaml from "@shikijs/langs/yaml";
import githubDarkDefault from "@shikijs/themes/github-dark-default";

const LANGUAGE_ALIASES: Record<string, string> = {
  bash: "shellscript",
  console: "shellscript",
  js: "javascript",
  md: "markdown",
  py: "python",
  rs: "rust",
  sh: "shellscript",
  ts: "typescript",
  txt: "text",
  yml: "yaml",
};

const SUPPORTED_LANGUAGES = new Set([
  "css", "html", "javascript", "json", "jsx", "markdown", "python", "rust",
  "shellscript", "tsx", "typescript", "yaml",
]);

const highlighter = createHighlighterCore({
  themes: [githubDarkDefault],
  langs: [css, html, javascript, json, jsx, markdown, python, rust, shellscript, tsx, typescript, yaml],
  engine: createOnigurumaEngine(getWasmInstance),
});

export async function highlight(code: string, language: string): Promise<ThemedToken[][]> {
  const requested = (LANGUAGE_ALIASES[language] ?? language) || "text";
  const lang = SUPPORTED_LANGUAGES.has(requested) ? requested : "text";
  const instance = await highlighter;
  return instance.codeToTokens(code, { lang, theme: "github-dark-default" }).tokens;
}
