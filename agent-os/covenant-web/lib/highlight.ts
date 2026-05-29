// Syntax highlighting via highlight.js with explicit language registration.
// Tree-shaking only kicks in when we register a fixed set of languages
// instead of pulling the full bundle — this trims ~700KB off the client
// chunk for the limited set a coding-agent run actually emits.

import hljs from "highlight.js/lib/core";
import bash from "highlight.js/lib/languages/bash";
import css from "highlight.js/lib/languages/css";
import javascript from "highlight.js/lib/languages/javascript";
import json from "highlight.js/lib/languages/json";
import markdown from "highlight.js/lib/languages/markdown";
import python from "highlight.js/lib/languages/python";
import rust from "highlight.js/lib/languages/rust";
import typescript from "highlight.js/lib/languages/typescript";
import xml from "highlight.js/lib/languages/xml";
import yaml from "highlight.js/lib/languages/yaml";

hljs.registerLanguage("bash", bash);
hljs.registerLanguage("css", css);
hljs.registerLanguage("javascript", javascript);
hljs.registerLanguage("json", json);
hljs.registerLanguage("markdown", markdown);
hljs.registerLanguage("python", python);
hljs.registerLanguage("rust", rust);
hljs.registerLanguage("typescript", typescript);
hljs.registerLanguage("xml", xml);
hljs.registerLanguage("yaml", yaml);

const EXTENSION_LANGUAGE: Record<string, string> = {
  ts: "typescript",
  tsx: "typescript",
  js: "javascript",
  jsx: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  py: "python",
  rs: "rust",
  json: "json",
  md: "markdown",
  markdown: "markdown",
  html: "xml",
  htm: "xml",
  xml: "xml",
  svg: "xml",
  css: "css",
  sh: "bash",
  bash: "bash",
  zsh: "bash",
  yaml: "yaml",
  yml: "yaml",
};

export function languageForPath(path: string): string | null {
  const tail = path.split("/").pop() ?? path;
  const dot = tail.lastIndexOf(".");
  if (dot < 0) return null;
  const ext = tail.slice(dot + 1).toLowerCase();
  return EXTENSION_LANGUAGE[ext] ?? null;
}

// Returns HTML — caller renders via `dangerouslySetInnerHTML`. Falls back
// to plain text (HTML-escaped) when no language matches, so the unknown-
// extension path never throws or surfaces raw markup.
export function highlightCode(content: string, language: string | null): string {
  if (language && hljs.getLanguage(language)) {
    return hljs.highlight(content, { language, ignoreIllegals: true }).value;
  }
  return escapeHtml(content);
}

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}
