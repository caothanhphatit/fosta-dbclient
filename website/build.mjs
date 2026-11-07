#!/usr/bin/env node
// Renders the Fosta Studio website for every language. Node 20+, no dependencies.
//
//   node website/build.mjs <outdir>    check the strings, then write <outdir>/index.html (English), <outdir>/vi/, /zh/, /ja/
//   node website/build.mjs --check     only check the strings and the rendered pages, write nothing
//
// index.html is the English template. Translatable text is marked with data-i18n="key" (the element's content is
// replaced) and data-i18n-attr="attr:key; attr:key" (attribute values are replaced); the strings live in i18n/<lang>.json.
// See README.md for the rules.

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createHash } from "node:crypto";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const ORIGIN = "https://dbcl.fosta.app";

// The first locale is the default: served at the root, and the x-default alternate.
export const LOCALES = [
  { code: "en", dir: "", htmlLang: "en", ogLocale: "en_US", name: "English", short: "EN" },
  { code: "vi", dir: "vi", htmlLang: "vi", ogLocale: "vi_VN", name: "Tiếng Việt", short: "VI" },
  { code: "zh", dir: "zh", htmlLang: "zh-Hans", ogLocale: "zh_CN", name: "简体中文", short: "中文" },
  { code: "ja", dir: "ja", htmlLang: "ja", ogLocale: "ja_JP", name: "日本語", short: "日本語" },
];

// Keys the build itself uses (language switcher, language banner); they are not in the template.
const BUILD_KEYS = ["lang.label", "lang.current", "offer.prompt", "offer.dismiss"];

// Files copied next to the pages. README.md files are documentation and are not published.
const STATIC = ["styles.css", "main.js", "assets"];
const MARKER = ".fosta-site"; // written into the output folder, so a rebuild may safely empty it

// Inline markup allowed inside a string, with the attributes each tag may carry.
const INLINE = { strong: [], b: [], em: [], code: [], kbd: [], br: [], a: ["href"] };
const VOID = new Set(["br"]);

class BuildError extends Error {}

// Short content hash of a shared file, appended to its URL in the pages (cache busting, see renderPage).
const versions = new Map();
function assetVersion(name) {
  if (!versions.has(name)) {
    const data = fs.readFileSync(path.join(HERE, name));
    versions.set(name, createHash("sha256").update(data).digest("hex").slice(0, 10));
  }
  return versions.get(name);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const decode = (s) =>
  s
    .replace(/&nbsp;/g, "\u00a0")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
    .replace(/&#(\d+);/g, (_, n) => String.fromCodePoint(Number(n)))
    .replace(/&amp;/g, "&");

// Compare template text with en.json loosely: entities decoded, runs of ASCII whitespace collapsed.
const normalize = (s) =>
  decode(s)
    .replace(/[ \t\r\n]+/g, " ")
    .trim();

const escapeAttr = (s) => s.replace(/&/g, "&amp;").replace(/"/g, "&quot;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

// Strips comments, keeping the <!-- i18n:... --> markers the build fills in.
const stripComments = (html) => html.replace(/[ \t]*<!--(?!\s*i18n:)[\s\S]*?-->[ \t]*\r?\n?/g, "");

function parseAttrSpec(spec) {
  return spec
    .split(";")
    .map((part) => part.trim())
    .filter(Boolean)
    .map((part) => {
      const i = part.indexOf(":");
      if (i < 1) throw new BuildError(`bad data-i18n-attr entry "${part}" (expected attr:key)`);
      return { attr: part.slice(0, i).trim(), key: part.slice(i + 1).trim() };
    });
}

function getAttr(attrs, name) {
  const m = new RegExp(`\\s${name}="([^"]*)"`).exec(attrs);
  return m ? m[1] : null;
}

function setAttr(attrs, name, value) {
  const re = new RegExp(`(\\s${name}=")[^"]*(")`);
  if (!re.test(attrs)) throw new BuildError(`attribute ${name} listed in data-i18n-attr but missing on the element`);
  return attrs.replace(re, (_, a, b) => a + escapeAttr(value) + b);
}

// Index of the "</tag>" that closes the element whose start tag ends at `from`.
function findClose(html, tag, from) {
  const re = new RegExp(`<(/?)${tag}(?=[\\s>/])[^>]*>`, "gi");
  re.lastIndex = from;
  let depth = 1;
  for (let m; (m = re.exec(html));) {
    depth += m[1] ? -1 : 1;
    if (depth === 0) return m.index;
  }
  throw new BuildError(`no closing </${tag}>`);
}

// Walks every start tag carrying data-i18n / data-i18n-attr and calls visit() for it.
function eachMarked(html, visit) {
  const tagRe = /<([a-zA-Z][\w-]*)(\s[^>]*?)?(\/?)>/g;
  for (let m; (m = tagRe.exec(html));) {
    const attrs = m[2] || "";
    const contentKey = getAttr(attrs, "data-i18n");
    const attrSpec = getAttr(attrs, "data-i18n-attr");
    if (contentKey === null && attrSpec === null) continue;
    const tag = m[1].toLowerCase();
    let inner = null;
    let end = tagRe.lastIndex;
    if (contentKey !== null) {
      end = findClose(html, tag, tagRe.lastIndex);
      inner = html.slice(tagRe.lastIndex, end);
      if (/\sdata-i18n(-attr)?="/.test(inner)) throw new BuildError(`data-i18n="${contentKey}" contains another marked element`);
    }
    const next = visit({ start: m.index, startEnd: tagRe.lastIndex, end, tag, attrs, selfClose: m[3], contentKey, attrSpec, inner });
    if (next !== undefined) tagRe.lastIndex = next;
  }
}

// Validates a translated fragment (allowlisted inline tags only) and returns safe HTML.
function sanitize(str, where) {
  if (typeof str !== "string") throw new BuildError(`${where}: not a string`);
  const stack = [];
  let out = "";
  for (const token of str.split(/(<[^>]*>)/)) {
    if (!token) continue;
    if (token.startsWith("<")) {
      const m = /^<(\/?)([a-z]+)((?:\s+[a-z-]+="[^"<>]*")*)\s*\/?>$/.exec(token);
      if (!m || !(m[2] in INLINE)) throw new BuildError(`${where}: tag not allowed: ${token}`);
      const [, closing, tag, attrText] = m;
      if (closing) {
        if (stack.pop() !== tag) throw new BuildError(`${where}: unbalanced </${tag}>`);
        out += `</${tag}>`;
        continue;
      }
      let attrs = "";
      for (const a of attrText.matchAll(/([a-z-]+)="([^"]*)"/g)) {
        if (!INLINE[tag].includes(a[1])) throw new BuildError(`${where}: attribute ${a[1]} not allowed on <${tag}>`);
        if (a[1] === "href" && !/^(https:\/\/|#)/.test(a[2])) throw new BuildError(`${where}: href must be https:// or #: ${a[2]}`);
        attrs += ` ${a[1]}="${escapeAttr(decode(a[2]))}"`;
      }
      if (!VOID.has(tag)) stack.push(tag);
      out += `<${tag}${attrs}>`;
    } else {
      if (/[<>]/.test(token)) throw new BuildError(`${where}: stray < or >`);
      out += token.replace(/&(?![a-zA-Z]+;|#\d+;|#x[0-9a-fA-F]+;)/g, "&amp;");
    }
  }
  if (stack.length) throw new BuildError(`${where}: unclosed <${stack.pop()}>`);
  return out;
}

// What must survive translation unchanged: code, key caps and link targets.
function fixedParts(fragment) {
  const parts = [];
  for (const m of fragment.matchAll(/<(code|kbd)>([\s\S]*?)<\/\1>/g)) parts.push(`<${m[1]}>${normalize(m[2])}`);
  for (const m of fragment.matchAll(/href="([^"]*)"/g)) parts.push(`href ${m[1]}`);
  return parts.sort().join("\n");
}

// ---------------------------------------------------------------------------
// Strings
// ---------------------------------------------------------------------------

export function readTemplate() {
  return stripComments(fs.readFileSync(path.join(HERE, "index.html"), "utf8").replace(/\r\n/g, "\n"));
}

// Every key the template uses, with its English text as written in the template.
export function templateStrings(html) {
  const strings = new Map();
  const add = (key, kind, text) => {
    if (!/^[a-z][\w.-]*$/i.test(key)) throw new BuildError(`bad key "${key}"`);
    const prev = strings.get(key);
    if (prev && normalize(prev.text) !== normalize(text)) throw new BuildError(`key "${key}" is used for different English texts`);
    if (prev && prev.kind !== kind) throw new BuildError(`key "${key}" is used both as content and as an attribute`);
    strings.set(key, { kind, text });
  };
  eachMarked(html, ({ tag, attrs, contentKey, attrSpec, inner }) => {
    // <title> holds plain text; other elements may hold allowlisted inline markup.
    if (contentKey !== null) add(contentKey, tag === "title" ? "text" : "html", inner);
    if (attrSpec !== null) {
      for (const { attr, key } of parseAttrSpec(attrSpec)) {
        const value = getAttr(attrs, attr);
        if (value === null) throw new BuildError(`data-i18n-attr names ${attr}, which the element does not have (key "${key}")`);
        add(key, "text", value);
      }
    }
  });
  return strings;
}

function loadDict(code) {
  const file = path.join(HERE, "i18n", `${code}.json`);
  let dict;
  try {
    dict = JSON.parse(fs.readFileSync(file, "utf8"));
  } catch (e) {
    throw new BuildError(`i18n/${code}.json: ${e.message}`);
  }
  if (!dict || typeof dict !== "object" || Array.isArray(dict)) throw new BuildError(`i18n/${code}.json: expected an object of "key": "text"`);
  return dict;
}

// The key check: every language has exactly the template's keys plus BUILD_KEYS, the English texts match the template,
// every string is safe, and code / key caps / links are identical to English.
export function checkStrings(html) {
  const strings = templateStrings(html);
  const expected = new Set([...strings.keys(), ...BUILD_KEYS]);
  const dicts = {};
  const problems = [];
  for (const { code } of LOCALES) {
    let dict;
    try {
      dict = loadDict(code);
    } catch (e) {
      problems.push(e.message);
      continue;
    }
    dicts[code] = dict;
    for (const key of expected) if (!(key in dict)) problems.push(`i18n/${code}.json: missing key "${key}"`);
    for (const key of Object.keys(dict)) if (!expected.has(key)) problems.push(`i18n/${code}.json: extra key "${key}" (not used by index.html)`);
    for (const [key, value] of Object.entries(dict)) {
      if (!expected.has(key)) continue;
      if (typeof value !== "string" || !value.trim()) {
        problems.push(`i18n/${code}.json: "${key}" must be a non-empty string`);
        continue;
      }
      const kind = strings.get(key)?.kind ?? "text";
      try {
        if (kind === "html") sanitize(value, `i18n/${code}.json "${key}"`);
        else if (/[<>]/.test(value)) throw new BuildError(`i18n/${code}.json "${key}": attribute text cannot contain < or >`);
      } catch (e) {
        problems.push(e.message);
      }
    }
  }
  const en = dicts.en;
  if (en) {
    for (const [key, { text }] of strings) {
      if (typeof en[key] === "string" && normalize(en[key]) !== normalize(text)) {
        problems.push(`i18n/en.json "${key}" differs from index.html; update both:\n    index.html: ${normalize(text)}\n    en.json:    ${normalize(en[key])}`);
      }
    }
    for (const { code } of LOCALES.slice(1)) {
      const dict = dicts[code];
      if (!dict) continue;
      for (const [key, { kind }] of strings) {
        if (kind !== "html" || typeof dict[key] !== "string" || typeof en[key] !== "string") continue;
        if (fixedParts(dict[key]) !== fixedParts(en[key])) {
          problems.push(`i18n/${code}.json "${key}": <code>, <kbd> and links must match English exactly`);
        }
      }
    }
  }
  if (problems.length) throw new BuildError(`string check failed:\n  - ${problems.join("\n  - ")}`);
  return { strings, dicts };
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

const urlOf = (loc) => `${ORIGIN}/${loc.dir ? loc.dir + "/" : ""}`;

// Relative link from one locale's page to another's, so the output also works from any folder.
function relHref(from, to) {
  const up = from.dir ? "../" : "";
  if (!to.dir) return up || "./";
  return `${up}${to.dir}/`;
}

function langLinks(loc, attrs = "") {
  return LOCALES.map((to) => {
    const current = to.code === loc.code ? ' aria-current="page"' : "";
    return `<li><a href="${relHref(loc, to)}" hreflang="${to.htmlLang}" lang="${to.htmlLang}" data-lang-choice="${to.code}"${current}${attrs}>${to.name}</a></li>`;
  }).join("");
}

function headerSwitch(loc, dict) {
  return (
    `<details class="lang-switch" data-lang-switch>` +
    `<summary aria-label="${escapeAttr(dict["lang.current"])}" title="${escapeAttr(dict["lang.label"])}">` +
    `<svg class="icon" aria-hidden="true"><use href="#i-globe" /></svg><span aria-hidden="true">${loc.short}</span></summary>` +
    `<ul class="lang-menu">${langLinks(loc)}</ul></details>`
  );
}

function footerSwitch(loc, dict) {
  return (
    `<nav class="footer-langs" aria-label="${escapeAttr(dict["lang.label"])}">` +
    `<svg class="icon" aria-hidden="true"><use href="#i-globe" /></svg><ul>${langLinks(loc)}</ul></nav>`
  );
}

// English page only: a hidden banner that main.js may show to visitors whose browser prefers another language.
function langOffer(loc, dicts) {
  if (loc !== LOCALES[0]) return "";
  const offers = LOCALES.slice(1)
    .map(
      (to) =>
        `<p lang="${to.htmlLang}" data-offer="${to.code}" data-dismiss="${escapeAttr(dicts[to.code]["offer.dismiss"])}" hidden>` +
        `<a href="${relHref(loc, to)}" hreflang="${to.htmlLang}" data-lang-choice="${to.code}">${escapeAttr(dicts[to.code]["offer.prompt"])}</a></p>`,
    )
    .join("");
  return (
    `<div class="lang-offer" data-lang-offer hidden><div class="container lang-offer-inner">` +
    `<svg class="icon" aria-hidden="true"><use href="#i-globe" /></svg>${offers}` +
    `<button type="button" class="lang-offer-close" data-lang-offer-close aria-label="Close">` +
    `<svg class="icon" aria-hidden="true"><use href="#i-x" /></svg></button></div></div>`
  );
}

function replaceMarker(html, name, content) {
  const re = new RegExp(`([ \\t]*)<!--\\s*i18n:${name}\\s*-->`);
  const m = re.exec(html);
  if (!m) throw new BuildError(`index.html is missing the <!-- i18n:${name} --> marker`);
  const indent = m[1];
  const body = content
    ? content
        .split("\n")
        .map((line) => indent + line)
        .join("\n")
    : "";
  return html.slice(0, m.index) + body + html.slice(m.index + m[0].length).replace(content ? /^/ : /^\r?\n/, "");
}

export function renderPage(html, loc, dicts) {
  const dict = dicts[loc.code];

  // 1. Strings.
  let out = "";
  let pos = 0;
  eachMarked(html, ({ start, startEnd, end, tag, attrs, selfClose, contentKey, attrSpec }) => {
    let newAttrs = attrs;
    if (attrSpec !== null) for (const { attr, key } of parseAttrSpec(attrSpec)) newAttrs = setAttr(newAttrs, attr, dict[key]);
    newAttrs = newAttrs.replace(/\s+data-i18n(-attr)?="[^"]*"/g, "");
    out += html.slice(pos, start) + `<${tag}${newAttrs}${selfClose}>`;
    if (contentKey !== null) {
      out += tag === "title" ? escapeAttr(decode(dict[contentKey])) : sanitize(dict[contentKey], `${loc.code} "${contentKey}"`);
      pos = end;
      return end;
    }
    pos = startEnd;
  });
  out += html.slice(pos);

  // 2. Language, canonical URL and alternates.
  const url = urlOf(loc);
  out = out.replace(/<html lang="[^"]*"/, `<html lang="${loc.htmlLang}"`);
  out = out.replace(/(<link rel="canonical" href=")[^"]*(")/, `$1${url}$2`);
  out = out.replace(/(<meta property="og:url" content=")[^"]*(")/, `$1${url}$2`);
  const alternates = [
    ...LOCALES.map((l) => `<link rel="alternate" hreflang="${l.htmlLang}" href="${urlOf(l)}" />`),
    `<link rel="alternate" hreflang="x-default" href="${urlOf(LOCALES[0])}" />`,
  ].join("\n");
  out = replaceMarker(out, "alternates", alternates);
  const ogLocales = [
    `<meta property="og:locale" content="${loc.ogLocale}" />`,
    ...LOCALES.filter((l) => l !== loc).map((l) => `<meta property="og:locale:alternate" content="${l.ogLocale}" />`),
  ].join("\n");
  out = replaceMarker(out, "og-locale", ogLocales);

  // 3. Language switcher and banner.
  out = replaceMarker(out, "lang-switch", headerSwitch(loc, dict));
  out = replaceMarker(out, "lang-footer", footerSwitch(loc, dict));
  out = replaceMarker(out, "lang-offer", langOffer(loc, dicts));

  // 4. Pages in a sub-folder reach the shared files one level up.
  if (loc.dir) out = out.replace(/(\s(?:href|src|data-shot)=")(assets\/|styles\.css|main\.js)/g, "$1../$2");

  // 5. Cache busting: the stylesheet and script URLs carry a hash of their content, so a CDN or browser cache never
  // pairs a new page with an old styles.css or main.js (that mix left the carousel slides stacked on phones).
  out = out.replace(/(\s(?:href|src)="(?:\.\.\/)?)(styles\.css|main\.js)"/g, (_, a, name) => `${a}${name}?v=${assetVersion(name)}"`);

  return out;
}

// Sanity checks on a rendered page.
function checkPage(page, loc, dicts) {
  const problems = [];
  if (/data-i18n/.test(page)) problems.push("leftover data-i18n attribute");
  if (/<!--\s*i18n:/.test(page)) problems.push("leftover i18n marker");
  if ((page.match(/<h1[\s>]/g) || []).length !== 1) problems.push("expected exactly one <h1>");
  for (const landmark of ["<header", "<main", "<footer", "<nav"]) if (!page.includes(landmark)) problems.push(`missing ${landmark}>`);
  if (!page.includes(`<html lang="${loc.htmlLang}"`)) problems.push("wrong <html lang>");
  if ((page.match(/hreflang="x-default"/g) || []).length !== 1) problems.push("expected one x-default alternate");
  if (problems.length) throw new BuildError(`${loc.code} page: ${problems.join("; ")}`);

  // Not an error: strings that are identical to English (often fine, e.g. product names), listed for review.
  if (loc === LOCALES[0]) return [];
  const same = [];
  for (const [key, value] of Object.entries(dicts[loc.code])) if (normalize(value) === normalize(dicts.en[key] ?? "")) same.push(key);
  return same;
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

function prepareOutDir(outDir) {
  const resolved = path.resolve(outDir);
  const forbidden = [HERE, path.dirname(HERE), path.parse(resolved).root].map((p) => path.resolve(p));
  if (forbidden.includes(resolved) || HERE.startsWith(resolved + path.sep)) {
    throw new BuildError(`refusing to build into ${resolved}`);
  }
  if (fs.existsSync(resolved)) {
    const entries = fs.readdirSync(resolved);
    if (entries.length && !entries.includes(MARKER)) {
      throw new BuildError(`${resolved} is not empty and was not created by this script; pick a new or empty folder`);
    }
    fs.rmSync(resolved, { recursive: true, force: true });
  }
  fs.mkdirSync(resolved, { recursive: true });
  fs.writeFileSync(path.join(resolved, MARKER), "Output of website/build.mjs; the folder is emptied on every build.\n");
  return resolved;
}

function main(argv) {
  const args = argv.slice(2);
  const checkOnly = args.includes("--check");
  const outArg = args.find((a) => !a.startsWith("--"));
  if (!checkOnly && !outArg) {
    console.error("usage: node website/build.mjs <outdir>   |   node website/build.mjs --check");
    return 2;
  }
  const major = Number(process.versions.node.split(".")[0]);
  if (major < 20) console.warn(`warning: Node ${process.versions.node}; Node 20+ is expected`);

  const html = readTemplate();
  const { strings, dicts } = checkStrings(html);
  const pages = LOCALES.map((loc) => {
    const page = renderPage(html, loc, dicts);
    const same = checkPage(page, loc, dicts);
    return { loc, page, same };
  });
  console.log(`strings ok: ${strings.size} template keys + ${BUILD_KEYS.length} build keys in ${LOCALES.length} languages`);
  for (const { loc, same } of pages) {
    if (same.length) console.log(`  ${loc.code}: same as English (check these are intentional): ${same.join(", ")}`);
  }
  if (checkOnly) return 0;

  const out = prepareOutDir(outArg);
  for (const name of STATIC) {
    fs.cpSync(path.join(HERE, name), path.join(out, name), {
      recursive: true,
      filter: (src) => path.basename(src) !== "README.md",
    });
  }
  for (const { loc, page } of pages) {
    const dir = path.join(out, loc.dir);
    fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(path.join(dir, "index.html"), page);
  }
  console.log(`wrote ${LOCALES.map((l) => "/" + (l.dir ? l.dir + "/" : "")).join(" ")} to ${out}`);
  return 0;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    process.exitCode = main(process.argv);
  } catch (e) {
    if (!(e instanceof BuildError)) throw e;
    console.error(`build failed: ${e.message}`);
    process.exitCode = 1;
  }
}
