const LANGUAGE_BY_EXTENSION = {
  css: "CSS",
  html: "HTML",
  htm: "HTML",
  js: "JavaScript",
  json: "JSON",
  jsx: "JavaScript React",
  md: "Markdown",
  mjs: "JavaScript",
  ts: "TypeScript",
  tsx: "TypeScript React",
  yaml: "YAML",
  yml: "YAML",
};

export function languageFor(path) {
  return LANGUAGE_BY_EXTENSION[path.split(".").pop()?.toLowerCase()] || "Plain text";
}

export function searchProjectFiles(files, query, { caseSensitive = false, regex = false } = {}) {
  if (!query) return { error: null, matchCount: 0, results: [] };
  let pattern;
  try {
    const source = regex ? query : query.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    pattern = new RegExp(source, caseSensitive ? "g" : "gi");
  } catch {
    return { error: "Invalid regular expression", matchCount: 0, results: [] };
  }

  const results = [];
  let matchCount = 0;
  for (const [path, content] of Object.entries(files).sort(([a], [b]) => a.localeCompare(b))) {
    const matches = [];
    const lines = content.split("\n");
    for (let index = 0; index < lines.length && matches.length < 200 && matchCount < 2000; index += 1) {
      pattern.lastIndex = 0;
      for (const match of lines[index].matchAll(pattern)) {
        if (!match[0]) break;
        matches.push({ line: index + 1, start: match.index || 0, end: (match.index || 0) + match[0].length, text: lines[index] });
        matchCount += 1;
        if (matches.length >= 200 || matchCount >= 2000) break;
      }
    }
    if (matches.length) results.push({ path, matches });
    if (matchCount >= 2000) break;
  }
  return { error: null, matchCount, results };
}

export function reorderItems(items, from, target, side = "after") {
  if (from === target || !items.includes(from) || !items.includes(target)) return items;
  const next = items.filter((item) => item !== from);
  const targetIndex = next.indexOf(target) + (side === "after" ? 1 : 0);
  next.splice(targetIndex, 0, from);
  return next.every((item, index) => item === items[index]) ? items : next;
}

export function highlightCode(path, source) {
  const language = languageFor(path);
  if (language === "HTML") return highlightByPattern(source, /<!--[\s\S]*?-->|<\/?[A-Za-z][^>]*>|&(?:[A-Za-z]+|#\d+);/g, htmlToken);
  if (language === "YAML") return highlightByPattern(source, /#[^\n]*|^[ \t-]*[A-Za-z_][\w.-]*(?=\s*:)|"(?:\\.|[^"\\])*"|'[^']*'|\b(?:true|false|null|\d+(?:\.\d+)?)\b/gm, yamlToken);
  if (language === "JSON") return highlightByPattern(source, /"(?:\\.|[^"\\])*"(?=\s*:)|"(?:\\.|[^"\\])*"|\b(?:true|false|null)\b|-?\b\d+(?:\.\d+)?\b/g, jsonToken);
  if (["JavaScript", "TypeScript", "JavaScript React", "TypeScript React", "CSS"].includes(language)) {
    return highlightByPattern(source, /\/\*[\s\S]*?\*\/|\/\/[^\n]*|`(?:\\.|[^`\\])*`|"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|\b(?:async|await|break|case|catch|class|const|continue|default|delete|do|else|export|extends|false|finally|for|from|function|if|import|in|instanceof|interface|let|new|null|of|return|static|super|switch|this|throw|true|try|type|typeof|undefined|var|void|while|yield)\b|\b\d+(?:\.\d+)?\b/g, scriptToken);
  }
  return escapeHtml(source);
}

export function lintDocument(path, source, files) {
  const diagnostics = [];
  if (/\.json$/i.test(path)) {
    try { JSON.parse(source); } catch (error) { diagnostics.push({ line: jsonErrorLine(source, error.message), message: error.message.replace(/^JSON\.parse: /, ""), severity: "error" }); }
  }
  if (/\.ya?ml$/i.test(path)) {
    source.split("\n").forEach((line, index) => {
      if (/\t/.test(line)) diagnostics.push({ line: index + 1, message: "YAML indentation must use spaces, not tabs.", severity: "error" });
    });
  }
  if (path === "manifest.yaml") {
    const fields = Object.fromEntries([...source.matchAll(/^([A-Za-z_][\w-]*):\s*["']?([^"'\n]*)/gm)].map((match) => [match[1], match[2].trim()]));
    for (const field of ["name", "version", "entrypoint"]) {
      if (!fields[field]) diagnostics.push({ line: 1, message: `Manifest ${field} is required.`, severity: "error" });
    }
    if (fields.entrypoint && files[fields.entrypoint] === undefined) diagnostics.push({ line: 1, message: `Entrypoint not found: ${fields.entrypoint}`, severity: "error" });
    if (fields.name && !/^[a-z0-9][a-z0-9._-]*$/.test(fields.name)) diagnostics.push({ line: 1, message: "Worker name must be URL-safe.", severity: "error" });
  }
  if (/\.(?:js|jsx|mjs|ts|tsx|css)$/i.test(path)) diagnostics.push(...lintDelimiters(source));
  if (/\.html?$/i.test(path)) diagnostics.push(...lintHtml(source));
  return diagnostics.slice(0, 100);
}

function escapeHtml(value) {
  return String(value).replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll('"', "&quot;").replaceAll("'", "&#039;");
}

function highlightByPattern(source, pattern, classifier) {
  let output = "";
  let cursor = 0;
  for (const match of source.matchAll(pattern)) {
    output += escapeHtml(source.slice(cursor, match.index));
    output += `<span class="token ${classifier(match[0])}">${escapeHtml(match[0])}</span>`;
    cursor = (match.index || 0) + match[0].length;
  }
  return output + escapeHtml(source.slice(cursor)) + (source.endsWith("\n") ? " " : "");
}

function htmlToken(value) { return value.startsWith("<!--") ? "comment" : value.startsWith("&") ? "entity" : "tag"; }
function yamlToken(value) { return value.startsWith("#") ? "comment" : /:$/.test(value) || /^[ \t-]*[A-Za-z_]/.test(value) && !/["']/.test(value) ? "property" : /^["']/.test(value) ? "string" : "literal"; }
function jsonToken(value) { return /^"/.test(value) ? (/"(?=\s*:)/.test(value) ? "property" : "string") : "literal"; }
function scriptToken(value) { return /^\/\//.test(value) || /^\/\*/.test(value) ? "comment" : /^[`"']/.test(value) ? "string" : /^\d/.test(value) ? "number" : "keyword"; }

function jsonErrorLine(source, message) {
  const offset = Number(message.match(/position (\d+)/)?.[1]);
  return Number.isFinite(offset) ? source.slice(0, offset).split("\n").length : 1;
}

function lintDelimiters(source) {
  const diagnostics = [];
  const stack = [];
  const pairs = { ")": "(", "]": "[", "}": "{" };
  let quote = "";
  let escaped = false;
  let line = 1;
  for (let index = 0; index < source.length; index += 1) {
    const char = source[index];
    const next = source[index + 1];
    if (char === "\n") line += 1;
    if (quote) {
      if (escaped) escaped = false;
      else if (char === "\\") escaped = true;
      else if (char === quote) quote = "";
      continue;
    }
    if (char === "/" && next === "/") { index = source.indexOf("\n", index); if (index < 0) break; line += 1; continue; }
    if (char === "/" && next === "*") { const end = source.indexOf("*/", index + 2); if (end < 0) { diagnostics.push({ line, message: "Unclosed block comment.", severity: "error" }); break; } line += source.slice(index, end).split("\n").length - 1; index = end + 1; continue; }
    if ('"\'`'.includes(char)) { quote = char; continue; }
    if ("([{ ".includes(char) && char !== " ") stack.push({ char, line });
    else if (pairs[char]) {
      const open = stack.pop();
      if (!open || open.char !== pairs[char]) diagnostics.push({ line, message: `Unexpected ${char}.`, severity: "error" });
    }
  }
  if (quote) diagnostics.push({ line, message: "Unclosed string literal.", severity: "error" });
  for (const open of stack.reverse()) diagnostics.push({ line: open.line, message: `Unclosed ${open.char}.`, severity: "error" });
  return diagnostics;
}

function lintHtml(source) {
  const diagnostics = [];
  const stack = [];
  const voidTags = new Set(["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr"]);
  for (const match of source.matchAll(/<\/?([A-Za-z][\w:-]*)\b[^>]*>/g)) {
    const tag = match[1].toLowerCase();
    const line = source.slice(0, match.index).split("\n").length;
    if (voidTags.has(tag) || /\/>$/.test(match[0])) continue;
    if (match[0].startsWith("</")) {
      const open = stack.pop();
      if (!open || open.tag !== tag) diagnostics.push({ line, message: `Unexpected closing tag </${tag}>.`, severity: "error" });
    } else stack.push({ tag, line });
  }
  for (const open of stack.reverse()) diagnostics.push({ line: open.line, message: `Unclosed <${open.tag}> tag.`, severity: "error" });
  return diagnostics;
}
