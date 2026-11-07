# DB Studio website

The marketing page for DB Studio, published at https://db.fosta.app. It is a static site with no build step and no dependencies:

```
website/
  index.html            the page
  styles.css            styles; tokens match the app (src/styles.css), light and dark via prefers-color-scheme
  main.js               progressive enhancement only: theme toggle and screenshot slots
  assets/
    logo.svg            app logo (same drawing as scripts/make-icons.ps1)
    favicon.svg
    og-image.png        1200×630 social preview (Open Graph / Twitter)
    screenshots/        optional PNGs that replace the HTML mocks, listed in shots.json; see screenshots/README.md
```

## Preview locally

From the repository root:

```powershell
npx serve website
```

and open the URL it prints. Opening `website/index.html` directly in a browser also works; the screenshot slots then
keep showing their mocks, because browsers block `fetch` of `shots.json` from `file://`.

## Deployment

The site is served at `https://db.fosta.app/` by nginx on the project's web server, from `/var/www/db.fosta.app`. That URL
is hard-coded in the canonical link and the Open Graph / Twitter tags in `index.html`.

To publish, run from the repository root with the server's SSH key (the key is never stored in the repository):

```sh
sh website/deploy.sh /path/to/key.pem            # default target: ubuntu@13.229.95.37
```

The script uploads into a staging folder and swaps it in, so visitors never see a half-copied site.

Server layout (installed once; kept next to the other sites without touching their files):

- `/etc/nginx/sites-available/db.fosta.app.conf` (+ link in `sites-enabled`): HTTP until DNS points at the server.
- `/etc/nginx/snippets/db.fosta.app-site.conf` and `db.fosta.app-headers.conf`: the static site, caching and security
  headers (CSP, nosniff, frame denial, HSTS).
- `/etc/nginx/db.fosta.app.tls.conf` and `/etc/nginx/db.fosta.app-enable-tls.sh`: once the DNS record exists, run
  `sudo /etc/nginx/db.fosta.app-enable-tls.sh` to get a Let's Encrypt certificate (webroot `/var/www/acme-db`, renewed by
  `certbot.timer`) and switch the site to HTTPS. Behind Cloudflare, use SSL mode *Full (strict)*.

## Editing notes

- Stick to facts from `README.md`, `ROADMAP.md` and `ARCHITECTURE.md`. Roadmap items are labelled "coming".
- Links to repository files must be absolute GitHub URLs (`https://github.com/caothanhphatit/fosta-dbclient/blob/main/...`),
  because the site is served from db.fosta.app, not from the repository.
- No third-party logos: engines are shown with generic labels and icons.
- When the first release ships, update the hero note and the "Build from source" card, and consider linking the Download
  button to `releases/latest`.
