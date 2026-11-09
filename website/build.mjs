#!/usr/bin/env node
// Renders the Fosta Studio website for every language. Node 20+, no dependencies.
//
//   node website/build.mjs <outdir>    check the strings, then write every page in every language: <outdir>/index.html
//                                      (English), <outdir>/databases/, <outdir>/donate/, and the same under /vi/, /zh/, /ja/
//   node website/build.mjs --check     only check the strings and the rendered pages, write nothing
//
// index.html, databases.html and donate.html are the English templates (see PAGES). Translatable text is marked with
// data-i18n="key" (the element's content is replaced) and data-i18n-attr="attr:key; attr:key" (attribute values are
// replaced); the strings of all pages live in one i18n/<lang>.json per language. See README.md for the rules.

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createHash } from "node:crypto";

const HERE = path.dirname(fileURLToPath(import.meta.url));
// The app version, from the repository's package.json: "{version}" in the page and its strings becomes this at build
// time, so the page always names the version the release workflow builds.
const APP_VERSION = JSON.parse(fs.readFileSync(path.join(HERE, "..", "package.json"), "utf8")).version;
const ORIGIN = "https://dbcl.fosta.app";

// The first locale is the default: served at the root, and the x-default alternate.
export const LOCALES = [
  { code: "en", dir: "", htmlLang: "en", ogLocale: "en_US", name: "English", short: "EN" },
  { code: "vi", dir: "vi", htmlLang: "vi", ogLocale: "vi_VN", name: "Tiếng Việt", short: "VI" },
  { code: "zh", dir: "zh", htmlLang: "zh-Hans", ogLocale: "zh_CN", name: "简体中文", short: "中文" },
  { code: "ja", dir: "ja", htmlLang: "ja", ogLocale: "ja_JP", name: "日本語", short: "日本語" },
];

// The pages: a template in this folder, rendered into a sub-folder of each language ("" is the language's root).
// Links between pages, and to assets/, styles.css and main.js, are written in the templates relative to the site root
// (as seen from index.html: "databases/", "./#features", "assets/logo.svg"); the build adds the "../" each page needs (see siteHref).
export const PAGES = [
  { template: "index.html", dir: "" },
  { template: "databases.html", dir: "databases" },
  { template: "donate.html", dir: "donate" },
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

export function readTemplate(name = "index.html") {
  return stripComments(fs.readFileSync(path.join(HERE, name), "utf8").replace(/\r\n/g, "\n"));
}

// Every key the templates use, with its English text as written in the template. `templates` is one HTML string or an
// object of template name to HTML; a key used in several templates must carry the same English text in all of them.
export function templateStrings(templates) {
  const strings = new Map();
  const add = (key, kind, text, where) => {
    if (!/^[a-z][\w.-]*$/i.test(key)) throw new BuildError(`${where}: bad key "${key}"`);
    const prev = strings.get(key);
    if (prev && normalize(prev.text) !== normalize(text)) throw new BuildError(`key "${key}" is used for different English texts (${prev.where}, ${where})`);
    if (prev && prev.kind !== kind) throw new BuildError(`key "${key}" is used both as content and as an attribute (${prev.where}, ${where})`);
    if (!prev) strings.set(key, { kind, text, where });
  };
  const all = typeof templates === "string" ? { "index.html": templates } : templates;
  for (const [where, html] of Object.entries(all)) {
    eachMarked(html, ({ tag, attrs, contentKey, attrSpec, inner }) => {
      // <title> holds plain text; other elements may hold allowlisted inline markup.
      if (contentKey !== null) add(contentKey, tag === "title" ? "text" : "html", inner, where);
      if (attrSpec !== null) {
        for (const { attr, key } of parseAttrSpec(attrSpec)) {
          const value = getAttr(attrs, attr);
          if (value === null) throw new BuildError(`${where}: data-i18n-attr names ${attr}, which the element does not have (key "${key}")`);
          add(key, "text", value, where);
        }
      }
    });
  }
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
export function checkStrings(templates) {
  const strings = templateStrings(templates);
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
    for (const key of Object.keys(dict)) if (!expected.has(key)) problems.push(`i18n/${code}.json: extra key "${key}" (not used by any template)`);
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
    for (const [key, { text, where }] of strings) {
      if (typeof en[key] === "string" && normalize(en[key]) !== normalize(text)) {
        problems.push(`i18n/en.json "${key}" differs from ${where}; update both:\n    ${where}: ${normalize(text)}\n    en.json:    ${normalize(en[key])}`);
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

// Folder of a page in a language, relative to the site root: "", "vi/", "databases/", "vi/databases/".
const folderOf = (loc, page) =>
  [loc.dir, page.dir]
    .filter(Boolean)
    .map((d) => d + "/")
    .join("");
const urlOf = (loc, page = PAGES[0]) => `${ORIGIN}/${folderOf(loc, page)}`;
// "../" once per folder level: the way from a page back to the site root.
const upOf = (loc, page) => "../".repeat([loc.dir, page.dir].filter(Boolean).length);

// Relative link from a page to the same page in another language, so the output also works from any folder.
function relHref(from, to, page) {
  return upOf(from, page) + folderOf(to, page) || "./";
}

// Rewrites a link that the template writes relative to the site root (see PAGES) for a page in a sub-folder. The shared
// files (assets/, styles.css, main.js) exist once, at the site root; links to pages stay in the page's language, so
// "databases/" on /vi/donate/ becomes "../databases/" (/vi/databases/). Absolute URLs, "#…", "/…" and "?…" are left alone.
const SHARED = /^(?:assets\/|styles\.css|main\.js)/;
function siteHref(value, loc, page) {
  if (/^(?:[a-z][a-z0-9+.-]*:|#|\/|\?)/i.test(value)) return value;
  const path = value.replace(/^\.\/?/, "");
  const up = SHARED.test(path) ? upOf(loc, page) : page.dir ? "../" : "";
  return up + path || "./";
}

function langLinks(loc, page, attrs = "") {
  return LOCALES.map((to) => {
    const current = to.code === loc.code ? ' aria-current="page"' : "";
    return `<li><a href="${relHref(loc, to, page)}" hreflang="${to.htmlLang}" lang="${to.htmlLang}" data-lang-choice="${to.code}"${current}${attrs}>${to.name}</a></li>`;
  }).join("");
}

function headerSwitch(loc, page, dict) {
  return (
    `<details class="lang-switch" data-lang-switch>` +
    `<summary aria-label="${escapeAttr(dict["lang.current"])}" title="${escapeAttr(dict["lang.label"])}">` +
    `<svg class="icon" aria-hidden="true"><use href="#i-globe" /></svg><span aria-hidden="true">${loc.short}</span></summary>` +
    `<ul class="lang-menu">${langLinks(loc, page)}</ul></details>`
  );
}

function footerSwitch(loc, page, dict) {
  return (
    `<nav class="footer-langs" aria-label="${escapeAttr(dict["lang.label"])}">` +
    `<svg class="icon" aria-hidden="true"><use href="#i-globe" /></svg><ul>${langLinks(loc, page)}</ul></nav>`
  );
}

// English pages only: a hidden banner that main.js may show to visitors whose browser prefers another language.
function langOffer(loc, page, dicts) {
  if (loc !== LOCALES[0]) return "";
  const offers = LOCALES.slice(1)
    .map(
      (to) =>
        `<p lang="${to.htmlLang}" data-offer="${to.code}" data-dismiss="${escapeAttr(dicts[to.code]["offer.dismiss"])}" hidden>` +
        `<a href="${relHref(loc, to, page)}" hreflang="${to.htmlLang}" data-lang-choice="${to.code}">${escapeAttr(dicts[to.code]["offer.prompt"])}</a></p>`,
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
  if (!m) throw new BuildError(`a template is missing the <!-- i18n:${name} --> marker`);
  const indent = m[1];
  const body = content
    ? content
        .split("\n")
        .map((line) => indent + line)
        .join("\n")
    : "";
  return html.slice(0, m.index) + body + html.slice(m.index + m[0].length).replace(content ? /^/ : /^\r?\n/, "");
}

export function renderPage(html, loc, dicts, page = PAGES[0]) {
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

  // 2. Pages in a sub-folder reach the other pages and the shared files through "../". This runs before the build's
  // own links (language switcher, banner) are inserted, since those are already relative to this page.
  if (upOf(loc, page)) out = out.replace(/(\s(?:href|src|data-shot|data-qr)=")([^"]*)(")/g, (_, a, value, b) => a + siteHref(value, loc, page) + b);

  // 3. Language, canonical URL and alternates.
  const url = urlOf(loc, page);
  out = out.replace(/<html lang="[^"]*"/, `<html lang="${loc.htmlLang}"`);
  out = out.replace(/(<link rel="canonical" href=")[^"]*(")/, `$1${url}$2`);
  out = out.replace(/(<meta property="og:url" content=")[^"]*(")/, `$1${url}$2`);
  const alternates = [
    ...LOCALES.map((l) => `<link rel="alternate" hreflang="${l.htmlLang}" href="${urlOf(l, page)}" />`),
    `<link rel="alternate" hreflang="x-default" href="${urlOf(LOCALES[0], page)}" />`,
  ].join("\n");
  out = replaceMarker(out, "alternates", alternates);
  const ogLocales = [
    `<meta property="og:locale" content="${loc.ogLocale}" />`,
    ...LOCALES.filter((l) => l !== loc).map((l) => `<meta property="og:locale:alternate" content="${l.ogLocale}" />`),
  ].join("\n");
  out = replaceMarker(out, "og-locale", ogLocales);

  // 4. Language switcher and banner.
  out = replaceMarker(out, "lang-switch", headerSwitch(loc, page, dict));
  out = replaceMarker(out, "lang-footer", footerSwitch(loc, page, dict));
  out = replaceMarker(out, "lang-offer", langOffer(loc, page, dicts));

  // 5. Cache busting: the stylesheet and script URLs carry a hash of their content, so a CDN or browser cache never
  // pairs a new page with an old styles.css or main.js (that mix left the carousel slides stacked on phones).
  out = out.replace(/(\s(?:href|src)="(?:\.\.\/)*)(styles\.css|main\.js)"/g, (_, a, name) => `${a}${name}?v=${assetVersion(name)}"`);

  // 6. The app version.
  out = out.replaceAll("{version}", APP_VERSION);

  return out;
}

// Sanity checks on a rendered page.
function checkPage(page, loc, where = PAGES[0]) {
  const problems = [];
  if (/data-i18n/.test(page)) problems.push("leftover data-i18n attribute");
  if (/<!--\s*i18n:/.test(page)) problems.push("leftover i18n marker");
  if ((page.match(/<h1[\s>]/g) || []).length !== 1) problems.push("expected exactly one <h1>");
  for (const landmark of ["<header", "<main", "<footer", "<nav"]) if (!page.includes(landmark)) problems.push(`missing ${landmark}>`);
  if (!page.includes(`<html lang="${loc.htmlLang}"`)) problems.push("wrong <html lang>");
  if ((page.match(/hreflang="x-default"/g) || []).length !== 1) problems.push("expected one x-default alternate");
  if (!page.includes(`<link rel="canonical" href="${urlOf(loc, where)}"`)) problems.push("wrong canonical URL");
  if (!page.includes(`<meta property="og:url" content="${urlOf(loc, where)}"`)) problems.push("wrong og:url");
  if (problems.length) throw new BuildError(`${loc.code} ${where.template}: ${problems.join("; ")}`);
}

// Not an error: strings that are identical to English (often fine, e.g. product names), listed for review.
function sameAsEnglish(loc, dicts) {
  return Object.entries(dicts[loc.code])
    .filter(([key, value]) => normalize(value) === normalize(dicts.en[key] ?? ""))
    .map(([key]) => key);
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

  const templates = Object.fromEntries(PAGES.map((p) => [p.template, readTemplate(p.template)]));
  const { strings, dicts } = checkStrings(templates);
  const pages = [];
  for (const p of PAGES) {
    for (const loc of LOCALES) {
      const page = renderPage(templates[p.template], loc, dicts, p);
      checkPage(page, loc, p);
      pages.push({ loc, p, page });
    }
  }
  console.log(`strings ok: ${strings.size} template keys (${PAGES.length} pages) + ${BUILD_KEYS.length} build keys in ${LOCALES.length} languages`);
  for (const loc of LOCALES.slice(1)) {
    const same = sameAsEnglish(loc, dicts);
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
  for (const { loc, p, page } of pages) {
    const dir = path.join(out, folderOf(loc, p));
    fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(path.join(dir, "index.html"), page);
  }
  console.log(`wrote ${pages.map(({ loc, p }) => "/" + folderOf(loc, p)).join(" ")} to ${out}`);
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
