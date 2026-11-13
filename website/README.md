# Fosta Studio website

The marketing page for Fosta Studio, published at https://dbcl.fosta.app in English (default, at `/`), Vietnamese (`/vi/`),
Simplified Chinese (`/zh/`) and Japanese (`/ja/`). It is a static site with no dependencies. A small Node script renders the
four languages from English templates; nothing is translated in the browser.

```
website/
  index.html            the landing page's English template (/, /vi/, /zh/, /ja/)
  databases.html        the Databases page (/databases/, /vi/databases/, …): feature matrix and one card per engine
  donate.html           the Support page (/donate/, /vi/donate/, …): ways to help and the donation QR codes
  i18n/
    en.json             English strings of all pages (must match the templates' text)
    vi.json zh.json ja.json
  build.mjs             renders every language into an output folder and runs the key check (Node 20+, no packages)
  styles.css            styles; tokens match the app (src/styles.css), light and dark via prefers-color-scheme,
                        per-language font stacks and CJK line breaking
  main.js               progressive enhancement only: theme toggle, hero carousel, language menu and banner, screenshot and QR slots
  deploy.sh             builds, then uploads the rendered output
  assets/
    logo.svg            app logo (same drawing as scripts/make-icons.ps1)
    favicon.svg
    og-image.png        1200×630 social preview (Open Graph / Twitter); its text is English on every language
    screenshots/        optional PNGs that replace the HTML mocks, listed in shots.json; see screenshots/README.md
    donate/             optional donation QR codes, listed in qr.json; see donate/README.md
```

## Pages

`PAGES` in `build.mjs` lists the templates and the folder each is rendered into; every page is rendered in every language
(`/databases/`, `/vi/databases/`, …). All templates share one set of strings per language, so the nav, footer and theme
keys are written once; a key used by several templates must have the same English text in each.

Links between pages and to `assets/`, `styles.css` and `main.js` are written in every template relative to the site root,
as seen from `index.html` (`databases/`, `donate/`, `./#features` for a landing page section, `./` for the home page,
`assets/logo.svg`). The build adds the `../` each page needs: shared files exist once at the site root
(`/vi/databases/` gets `../../assets/logo.svg`), while links to pages stay in the page's language (`databases/` on
`/vi/donate/` becomes `../databases/`, which is `/vi/databases/`). Absolute URLs and `#…` links are left alone.

To add a page: copy `databases.html` (head, sprite, header and footer included), give it its own `<title>`, description,
canonical URL and `og:url` (the build rewrites the last two per language), add it to `PAGES`, link it from the header nav
and footer of every template, add its strings, and run the check.

- **Databases** (`databases.html`): a feature matrix and a card per engine with the tested version, what works, engine
  views, vector tools and notes. Facts come from `docs/engines/*.md`, `docs/features.md` and `CHANGELOG.md`; "Tested with"
  is the version the CI contract suites run (`ci/engines/*.json`, `versions`; SQLite is the bundled library from
  `docs/engines/sqlite.md`). Update the matrix and the cards when an engine gains a feature. Cards link to
  `docs/engines/<id>.md`, or to `docs/features.md` for the engines without their own notes.
- **Support** (`donate.html`): why donations help, ways to help without money, and two QR code slots. They show "QR code
  coming soon" until the PNG is added; see `assets/donate/README.md`.

The app itself is English only, so the HTML mocks of the app keep its English interface (buttons, labels, data). Their
descriptions (`aria-label`, captions) and the chat text in the assistant panels are translated.

## Preview locally

From the repository root:

```powershell
node website/build.mjs .site
npx serve .site
```

and open the URL it prints (`/`, `/vi/`, `/zh/`, `/ja/`, and `databases/` or `donate/` under each). `.site/` is git-ignored. The build empties its output folder
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

The build also sets `<html lang>`, the canonical URL, `og:url` and the hreflang alternates per page and language, and adds
`../` to site-relative links on the pages in sub-folders (see [Pages](#pages)).

### Edit a string

1. Change the text in `index.html` **and** the same key in `i18n/en.json` (the check fails if they differ, so the template
   stays a readable English page).
2. Update the key in `vi.json`, `zh.json` and `ja.json`.
3. Run `node website/build.mjs --check`.

### Add a string

Mark the element in the template (`index.html`, `databases.html` or `donate.html`) with a new key (`section.thing`), add the key to all four JSON files, run the check. Keys
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

The hero shows four app views in one window frame: the SQL grid with the assistant, the ER diagram, the workspace commands and
the command editor (the last two are screenshot slots, `commands.png` and `commands-dialog.png`). Without JavaScript it is a
horizontally scrollable, snapping strip that starts on the first view. `main.js` adds arrows, dots, a pause button and
auto-advance every 6 s; it pauses on hover, on keyboard focus inside the carousel and in background tabs, and with
`prefers-reduced-motion` it neither auto-advances nor animates. `#slide=2` (or 3, 4) in the URL opens that view with
auto-advance paused, which is handy for links and screenshots.

MongoDB is not in the carousel: it has its own feature block further down the page (`mongo.*` keys), a collection workspace
mock (`mock-mongo`) with an optional screenshot slot, `assets/screenshots/mongo.png`.

On phones (600 px and below) each slide shows only the main area of the window: the workspace rail, the tree and the
assistant panel are hidden, and the ER diagram switches to a two-column version of the schema so its text stays readable.

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
- Engine logos in the home page chip strip are Simple Icons marks (CC0, v16.35.0), inlined as `i-db-*` symbols; Oracle and SQL
  Server are not in Simple Icons and use the app's own glyphs. They sit on a light tile in both themes. Keep the trademark
  line in the footer (`footer.trademarks`).
- "Download for Windows" links straight to the installer: `build.mjs` builds the asset URLs from the version in
  `package.json` (`INSTALLER_URL`, `PORTABLE_URL`; `{installer-url}` / `{portable-url}` in the templates), so a release
  only needs the version bump, and the site is deployed after the release is published (the URLs 404 before that). The
  click also opens a panel with the portable build, release notes and the SmartScreen tip.
- When the first release ships, update the "Build from source" card (in every language).
