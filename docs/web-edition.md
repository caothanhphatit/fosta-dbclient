# Web edition (Docker)

The web edition runs the same backend and UI in a container; you use it in a browser.

```sh
docker run -d --name fosta-studio -p 127.0.0.1:8080:8080 -v fosta-studio-data:/data ghcr.io/caothanhphatit/fosta-studio
docker logs fosta-studio
```

To build the image yourself: `docker build -t fosta-studio .` and use `fosta-studio` as the image name.

1. **Sign in**: on first start the log prints a sign-in address with a random access token, e.g.
   `http://localhost:8080/?token=3f9c…`. Open it; the token is exchanged for a session cookie (30 days) and removed from the address.
   The token is kept in the data volume; print the address again with
   `docker exec fosta-studio /app/fosta-studio-server token`.
   Signing out (`POST /api/logout`) ends **every** session, in all browsers: there is one access token per server, not per user.
   If the token may have leaked, replace it with `docker exec fosta-studio /app/fosta-studio-server rotate-token`: the old token
   and all sessions stop working at once, without a restart, and the new sign-in address is printed. (With `FOSTA_STUDIO_TOKEN`
   set, change that variable and recreate the container instead.)
2. **Data**: workspaces, connections, query history, the access token and the encrypted saved passwords live in the `/data` volume.
   Saved passwords are encrypted with a key that is also stored there (`fosta-studio-secret.key`) unless you set
   `FOSTA_STUDIO_SECRET_KEY`; **if the key is lost, saved passwords cannot be recovered** (re-enter them).
3. **Databases on your machine**: inside the container, `localhost` is the container itself. Use `host.docker.internal` as the
   host (Docker Desktop provides it; on Linux add `--add-host=host.docker.internal:host-gateway` to `docker run`). The connection
   dialog shows this hint when you type `localhost` or `127.0.0.1`.
4. **Updating**: see [Updating](#updating); everything in the volume is kept.

`deploy/docker-compose.yml` runs the same container, optionally with a MariaDB to try it out
(`docker compose -f deploy/docker-compose.yml --profile demo up -d`).

| Variable                       | Default                     | Meaning                                                                                                     |
| ------------------------------ | --------------------------- | ----------------------------------------------------------------------------------------------------------- |
| `FOSTA_STUDIO_TOKEN`           | generated, in `/data`       | Access token to sign in with (at least 16 characters).                                                      |
| `FOSTA_STUDIO_AUTH`            | `token`                     | `off` disables sign-in. Only for a trusted machine or behind your own authenticating proxy; logs a warning. |
| `FOSTA_STUDIO_SECRET_KEY`      | generated, in `/data`       | 64 hex characters (`openssl rand -hex 32`) that encrypt saved passwords and API keys.                       |
| `FOSTA_STUDIO_DATA`            | `/data`                     | Data directory.                                                                                             |
| `FOSTA_STUDIO_LISTEN`          | `0.0.0.0:8080` in the image | Address and port to listen on.                                                                              |
| `FOSTA_STUDIO_PUBLIC_URL`      | `http://localhost:<port>`   | Base of the sign-in address printed in the log.                                                             |
| `FOSTA_STUDIO_ALLOWED_ORIGINS` | none                        | Extra origins allowed to call the API, comma-separated (e.g. `https://studio.example.com` behind a proxy).  |
| `FOSTA_STUDIO_ALLOWED_HOSTS`   | any (loopback if auth off)  | Host names requests may use, comma-separated.                                                               |
| `FOSTA_STUDIO_SECURE_COOKIE`   | off                         | `on` marks the session cookie `Secure`; set it when an HTTPS proxy is in front.                             |
| `FOSTA_STUDIO_TASKS`           | off                         | `on` allows workspace commands. They run inside the container, which has no extra programs (no `kubectl`).  |

Differences from the desktop app: exports and diagram images are downloaded by the browser, there is no "Show in folder", workspace
commands are off unless `FOSTA_STUDIO_TASKS=on`, saved passwords are in the encrypted file instead of Windows Credential Manager, and
assistant requests go from the server to the provider. There is one access token per server, not separate user accounts.
For access over a network, put the server behind HTTPS (a reverse proxy) and set `FOSTA_STUDIO_SECURE_COOKIE=on`.

## Updating

The app checks GitHub for a new release once a day (Settings → Check for updates) and shows *Update X.Y.Z* in the status bar. It
never touches Docker itself. To update by hand, pull the image and re-create the container on the same volume:

```sh
docker pull ghcr.io/caothanhphatit/fosta-studio
docker rm -f fosta-studio
docker run -d --name fosta-studio -p 127.0.0.1:8080:8080 -v fosta-studio-data:/data ghcr.io/caothanhphatit/fosta-studio
```

With Compose: `docker compose -f deploy/docker-compose.yml pull` then `docker compose -f deploy/docker-compose.yml up -d`.

To update automatically, the Compose file has an optional [Watchtower](https://containrrr.dev/watchtower/) service that updates only
the `fosta-studio` container, once a day:

```sh
docker compose -f deploy/docker-compose.yml --profile autoupdate up -d
```

## Oracle in the web edition

The first Oracle connection offers to download Oracle Instant Client into the data volume (`/data/oracle`); it loads at once,
because the server puts that folder on `LD_LIBRARY_PATH` when it starts, and the image includes `libaio`. To use your own client
instead, put it on `LD_LIBRARY_PATH` (for example with `-e LD_LIBRARY_PATH=/data/my-client`) before the container starts. See [engines/oracle.md](engines/oracle.md).
