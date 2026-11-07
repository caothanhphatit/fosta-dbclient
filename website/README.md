# Fosta Studio website

The marketing page for Fosta Studio, published at https://dbcl.fosta.app in English (default, at `/`), Vietnamese (`/vi/`),
Simplified Chinese (`/zh/`) and Japanese (`/ja/`). It is a static site with no dependencies. A small Node script renders the
four languages from one English template; nothing is translated in the browser.

```
website/
  index.html            the English template (also the source of the page structure for every language)
  i18n/
    en.json             English strings (must match the template's text)
    vi.json zh.json ja.json
  build.mjs             renders every language into an output folder and runs the key check (Node 20+, no packages)
  styles.css            styles; tokens match the app (src/styles.css), light and dark via prefers-color-scheme,
                        per-language font stacks and CJK line breaking
  main.js               progressive enhancement only: theme toggle, hero carousel, language menu and banner, screenshot slots
  deploy.sh             builds, then uploads the rendered output
  assets/
    logo.svg            app logo (same drawing as scripts/make-icons.ps1)
    favicon.svg
    og-image.png        1200×630 social preview (Open Graph / Twitter); its text is English on every language
    screenshots/        optional PNGs that replace the HTML mocks, listed in shots.json; see screenshots/README.md
```

The app itself is English only, so the HTML mocks of the app keep its English interface (buttons, labels, data). Their
descriptions (`aria-label`, captions) and the chat text in the assistant panels are translated.

## Preview locally

From the repository root:

```powershell
node website/build.mjs .site
npx serve .site
```

and open the URL it prints (`/`, `/vi/`, `/zh/`, `/ja/`). `.site/` is git-ignored. The build empties its output folder
first, so it refuses to write into a folder it did not create (pick a new or empty one).

To check the strings without writing anything: `node website/build.mjs --check`.

## Translations

### How the template is marked

- `data-i18n="key"` on an element: its content is replaced by the string. Inline markup is allowed in strings, limited to
  `<strong>`, `<b>`, `<em>`, `<code>`, `<kbd>`, `<br>` and `<a href="https://…">` / `<a href="#…">`; anything else fails the
  build. A bare `&` is fine; `&nbsp;` and other entities are kept.
- `data-i18n-attr="aria-label:key; content:other.key"` on an element: those attribute values are replaced (plain text).
- `<title>` content is plain text.
- `<!-- i18n:… -->` comments are filled in by the build: `alternates` (hreflang links), `og-locale`, `lang-switch` (header
  menu), `lang-footer` (footer links) and `lang-offer` (the English page's language banner). Other comments are dropped.

The build also sets `<html lang>`, the canonical URL and `og:url` per language, and rewrites `assets/…`, `styles.css` and
`main.js` to `../…` on the pages in sub-folders.

### Edit a string

1. Change the text in `index.html` **and** the same key in `i18n/en.json` (the check fails if they differ, so the template
   stays a readable English page).
2. Update the key in `vi.json`, `zh.json` and `ja.json`.
3. Run `node website/build.mjs --check`.

### Add a string

Mark the element in `index.html` with a new key (`section.thing`), add the key to all four JSON files, run the check. Keys
used in several places must carry the same English text everywhere.

### The key check

`build.mjs` fails, and `deploy.sh` uploads nothing, when:

- a language file misses a key the template uses, or has a key nobody uses;
- `en.json` differs from the template text;
- a string contains a tag or attribute outside the allowlist, or unbalanced tags;
- a translation changes what is inside `<code>` / `<kbd>` or a link target (code, commands, file names and URLs stay
  identical in every language);
- a rendered page has a leftover `data-i18n` attribute or marker, not exactly one `<h1>`, a missing landmark, or wrong
  `lang` / hreflang.

It also lists strings that are identical to English, for review (often intentional, e.g. "Windows Credential Manager" in
Vietnamese).

### Translation conventions

- Keep product and engine names, code, SQL keywords, commands, file names, "GPL-3.0" and "CLA" as they are. Keep the app's
  own UI labels in English (Insert, New tab, Open / Guarded / Read-only / Custom, Production, List / JSON / Table), since
  the app is English.
- Use the terms developers use locally: Vietnamese keeps words like workspace, query, connection string, transaction in
  English; Chinese uses 工作区 / 连接 / 查询; Japanese uses ワークスペース / 接続 / クエリ.
- Same facts as the English page, no new claims; roadmap items stay marked as coming in every language.

### Add a language

1. Add an entry to `LOCALES` in `build.mjs` (code, folder, `htmlLang` as a BCP 47 tag, `ogLocale`, native name, short
   label for the header button).
2. Copy `i18n/en.json` to `i18n/<code>.json` and translate it, including `lang.current` ("Language: <name>"),
   `offer.prompt` (the banner offering this language on the English page) and `offer.dismiss`.
3. If the script needs different fonts or line breaking, add a `:root:lang(…)` block in the "Languages" part of
   `styles.css`. To offer the language in the English page's banner, add its code to `OFFERED` in `main.js`.
4. Run the check and look at the page at 1440 px and 375 px, light and dark.

### Language choice

The header menu and footer list link to every language (each link has `lang` and `hreflang`). A click is remembered in
`localStorage` (`fosta-studio-site-lang`). On the English page only, `main.js` shows a small dismissible banner ("Xem bằng
tiếng Việt?") when the visitor previously picked another language, or has never picked one and their browser prefers
Vietnamese, Chinese (Simplified) or Japanese over English. Nothing ever redirects.

## Hero carousel

The hero shows three app views (SQL grid, MongoDB documents, ER diagram) in one window frame. Without JavaScript it is a
horizontally scrollable, snapping strip that starts on the first view. `main.js` adds arrows, dots, a pause button and
auto-advance every 6 s; it pauses on hover, on keyboard focus inside the carousel and in background tabs, and with
`prefers-reduced-motion` it neither auto-advances nor animates. The MongoDB view previews the document view and query bar
from the roadmap, and its caption says so. `#slide=2` (or 3) in the URL opens that view with auto-advance paused, which is
handy for links and screenshots.

On phones (600 px and below) each slide shows only the main area of the window: the workspace rail, the tree and the
assistant panel are hidden, the MongoDB view drops its secondary buttons, and the ER diagram switches to a two-column
version of the schema so its text stays readable.

## Effects

All effects decorate a page that is already complete: without JavaScript, or with `prefers-reduced-motion`, the final state
is shown and nothing moves.

- Hero backdrop: faint table rules (rows and columns) behind the title, masked to fade out (CSS only).
- Scroll reveal: section headings, cards and feature rows fade up once when they scroll into view (`IntersectionObserver`).
- Numbers band in "Why": the figures count up once when visible, and always end on the real number.
- Query demo in "Features": the query is typed into the editor, Run is pressed and the rows arrive in the grid; it replays
  only while it stays on screen.
- ER diagram slide: the relation lines flow from the "one" side to the "many" side (CSS animation).

## Caching

`build.mjs` adds a short content hash to the `styles.css` and `main.js` URLs (`styles.css?v=…`). The CDN keeps those files
for hours, so without it a phone could get the new page with an old stylesheet (which is how the carousel once showed its
slides stacked on iOS). Never link the two files without going through the build.

## Deployment

The site is served at `https://dbcl.fosta.app/` by nginx on the project's web server, from `/var/www/db.fosta.app`. That
origin is set in `build.mjs` (`ORIGIN`) and in the Open Graph / Twitter image URLs in `index.html`.

To publish, run from the repository root with the server's SSH key (the key is never stored in the repository):

```sh
sh website/deploy.sh /path/to/key.pem            # default target: ubuntu@13.229.95.37
```

The script renders the site into a temporary folder with `build.mjs` (stopping if the key check fails), uploads only that
output (no template, `i18n/`, build script or READMEs) into a staging folder and swaps it in, so visitors never see a
half-copied site.

Server layout (installed once; kept next to the other sites without touching their files):

- `/etc/nginx/sites-available/db.fosta.app.conf` (+ link in `sites-enabled`): HTTPS for `dbcl.fosta.app` (and
  `db.fosta.app` once that name has a DNS record), HTTP redirected to HTTPS, Let's Encrypt certificate `dbcl.fosta.app`
  (webroot `/var/www/acme-db`, renewed by `certbot.timer`). To add `db.fosta.app` later:
  `sudo certbot certonly --webroot -w /var/www/acme-db --cert-name dbcl.fosta.app --expand -d dbcl.fosta.app -d db.fosta.app`.
- `/etc/nginx/snippets/db.fosta.app-site.conf` and `db.fosta.app-headers.conf`: the static site, caching and security
  headers (CSP, nosniff, frame denial, HSTS). The language folders are plain directories with an `index.html` each.
- DNS is on Cloudflare (proxied); keep the zone's SSL/TLS mode on _Full (strict)_.

## Editing notes

- Stick to facts from `README.md`, `ROADMAP.md` and `ARCHITECTURE.md`. Roadmap items are labelled "coming".
- Links to repository files must be absolute GitHub URLs (`https://github.com/caothanhphatit/fosta-dbclient/blob/main/...`),
  because the site is served from dbcl.fosta.app, not from the repository.
- No third-party logos: engines are shown with generic labels and icons.
- When the first release ships, update the hero note and the "Build from source" card (in every language), and consider
  linking the Download button to `releases/latest`.
