//! The web front-end (`fosta-studio-server`, feature `web`): serves the UI and exposes the shared commands
//! over HTTP, for the Docker image.
//!
//! - `POST /api/invoke/<command>`: the same JSON arguments and result as Tauri's `invoke` (see `api.rs`);
//!   `ai_chat` streams its events as NDJSON in the response.
//! - `GET /api/events`: Server-Sent Events for `task-status` / `task-log` (Tauri's `listen`).
//! - `GET /healthz`: liveness, without authentication.
//! - everything else: the built UI, behind the access token (see `auth.rs`).
//!
//! Configuration is read from environment variables; see `Settings::from_env` and the README.

mod api;
mod auth;
mod web;

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
};

use fosta_studio::{
    config,
    secrets::{
        self,
        file::{FileStore, KeySource, KEY_ENV},
    },
    AppState, Capabilities,
};
use tokio::sync::broadcast;

use crate::{api::WebEvents, auth::Auth};

const DEFAULT_LISTEN: &str = "127.0.0.1:8080";

/// Everything the server is configured with, from `FOSTA_STUDIO_*` environment variables.
struct Settings {
    listen: SocketAddr,
    ui_dir: PathBuf,
    data_dir: PathBuf,
    token: Option<String>,
    auth_off: bool,
    tasks: bool,
    secure_cookie: bool,
    public_url: Option<String>,
    allowed_origins: Vec<String>,
    allowed_hosts: Vec<String>,
}

impl Settings {
    fn from_env() -> Result<Self, String> {
        let listen = env("FOSTA_STUDIO_LISTEN").unwrap_or_else(|| DEFAULT_LISTEN.to_owned());
        let listen = listen
            .parse()
            .map_err(|_| format!("FOSTA_STUDIO_LISTEN must be an address and port such as 0.0.0.0:8080, not '{listen}'"))?;
        let auth = env("FOSTA_STUDIO_AUTH").unwrap_or_default().to_ascii_lowercase();
        if !matches!(auth.as_str(), "" | "on" | "token" | "off") {
            return Err(format!("FOSTA_STUDIO_AUTH must be 'token' (the default) or 'off', not '{auth}'"));
        }
        Ok(Self {
            listen,
            ui_dir: env("FOSTA_STUDIO_UI").map_or_else(|| PathBuf::from("dist"), PathBuf::from),
            data_dir: config::data_dir().map_err(|error| error.to_string())?,
            token: env("FOSTA_STUDIO_TOKEN"),
            auth_off: auth == "off",
            tasks: flag("FOSTA_STUDIO_TASKS"),
            secure_cookie: flag("FOSTA_STUDIO_SECURE_COOKIE"),
            public_url: env("FOSTA_STUDIO_PUBLIC_URL").map(|url| url.trim_end_matches('/').to_owned()),
            allowed_origins: list("FOSTA_STUDIO_ALLOWED_ORIGINS")
                .into_iter()
                .map(|origin| origin.trim_end_matches('/').to_owned())
                .collect(),
            allowed_hosts: list("FOSTA_STUDIO_ALLOWED_HOSTS"),
        })
    }

    /// The URL printed with the token: `FOSTA_STUDIO_PUBLIC_URL`, else localhost on the listening port.
    fn login_base(&self) -> String {
        self.public_url
            .clone()
            .unwrap_or_else(|| format!("http://localhost:{}", self.listen.port()))
    }
}

fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn flag(name: &str) -> bool {
    env(name).is_some_and(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "on" | "true" | "yes"))
}

fn list(name: &str) -> Vec<String> {
    env(name)
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// The server answers inside a container (Docker or Podman): `localhost` there is not the user's machine.
fn in_container() -> bool {
    Path::new("/.dockerenv").exists() || Path::new("/run/.containerenv").exists()
}

fn main() -> ExitCode {
    let command = std::env::args().nth(1);
    let settings = match Settings::from_env() {
        Ok(settings) => settings,
        Err(error) => {
            eprintln!("fosta-studio-server: {error}");
            return ExitCode::FAILURE;
        }
    };
    let result = match command.as_deref() {
        None | Some("serve") => serve(settings),
        Some("healthcheck") => web::healthcheck(settings.listen),
        Some("token") => print_login(&settings),
        Some("rotate-token") => rotate_token(&settings),
        Some("--version" | "version") => {
            println!("fosta-studio-server {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some(other) => Err(format!(
            "unknown command '{other}'. Usage: fosta-studio-server [serve | token | rotate-token | healthcheck | version]"
        )),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("fosta-studio-server: {error}");
            ExitCode::FAILURE
        }
    }
}

/// `fosta-studio-server token`: prints the login URL again (e.g. `docker exec <container> /app/fosta-studio-server token`).
fn print_login(settings: &Settings) -> Result<(), String> {
    if settings.auth_off {
        println!("Authentication is off (FOSTA_STUDIO_AUTH=off): open {}/", settings.login_base());
        return Ok(());
    }
    let (token, _) = auth::load_token(&settings.data_dir, settings.token.clone())?;
    println!("{}/?token={token}", settings.login_base());
    Ok(())
}

/// `fosta-studio-server rotate-token`: replaces the access token and the cookie signing key, which signs out
/// every browser; a running server picks up the new token on its next request.
fn rotate_token(settings: &Settings) -> Result<(), String> {
    if settings.auth_off {
        return Err(String::from(
            "authentication is off (FOSTA_STUDIO_AUTH=off): there is no token to rotate",
        ));
    }
    std::fs::create_dir_all(&settings.data_dir)
        .map_err(|error| format!("cannot create the data directory {}: {error}", settings.data_dir.display()))?;
    let token = auth::rotate_token(&settings.data_dir, settings.token.as_deref())?;
    println!("New access token saved; every browser is signed out. Sign in again at:");
    println!("{}/?token={token}", settings.login_base());
    Ok(())
}

fn serve(settings: Settings) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("cannot start the async runtime: {error}"))?;
    runtime.block_on(run(settings))
}

async fn run(settings: Settings) -> Result<(), String> {
    std::fs::create_dir_all(&settings.data_dir)
        .map_err(|error| format!("cannot create the data directory {}: {error}", settings.data_dir.display()))?;
    println!("Fosta Studio {} web edition", env!("CARGO_PKG_VERSION"));
    println!("Data directory: {}", settings.data_dir.display());
    let log_dir = fosta_studio::logging::init(&settings.data_dir, fosta_studio::logging::LogLocation::Web);
    fosta_studio::runtime::install_panic_hook();
    println!("Logs: {}", log_dir.display());

    let (store, key_source) =
        FileStore::open(&settings.data_dir, env(KEY_ENV).as_deref()).map_err(|error| format!("secret store: {error}"))?;
    match key_source {
        KeySource::Environment => println!("Saved passwords are encrypted with the key from {KEY_ENV}."),
        KeySource::ExistingFile(path) => println!("Saved passwords are encrypted with the key in {}.", path.display()),
        KeySource::GeneratedFile(path) => println!(
            "Generated {}: it encrypts saved passwords. Keep it (or set {KEY_ENV}); without it they cannot be read.",
            path.display()
        ),
    }
    secrets::install(Box::new(store)).map_err(|error| error.to_string())?;

    let auth = if settings.auth_off {
        warn_auth_off();
        Auth::off()
    } else {
        let (auth, token, generated) = Auth::open(&settings.data_dir, settings.token.clone(), settings.secure_cookie)?;
        announce_token(&settings, &token, generated);
        auth
    };
    if settings.tasks {
        println!("Startup tasks are on (FOSTA_STUDIO_TASKS): the UI can run programs inside this container.");
    }

    let (sender, _) = broadcast::channel(1024);
    let events = Arc::new(WebEvents(sender.clone()));
    let capabilities = Capabilities::web(settings.tasks, in_container());
    let state = AppState::new(capabilities, tokio::runtime::Handle::current(), events).map_err(|error| error.to_string())?;
    let state = Arc::new(state);

    if !settings.ui_dir.join("index.html").exists() {
        eprintln!(
            "Warning: no UI found in {} (set FOSTA_STUDIO_UI to the folder `npm run build` writes, dist/).",
            settings.ui_dir.display()
        );
    }
    let app = web::router(web::Server {
        state: Arc::clone(&state),
        events: sender,
        auth,
        allowed_origins: settings.allowed_origins.clone(),
        allowed_hosts: web::allowed_hosts(&settings.allowed_hosts, settings.auth_off),
        ui_dir: settings.ui_dir.clone(),
    });

    let listener = tokio::net::TcpListener::bind(settings.listen)
        .await
        .map_err(|error| format!("cannot listen on {}: {error}", settings.listen))?;
    println!("Listening on http://{}", settings.listen);
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|error| format!("server error: {error}"))?;
    state.tasks.stop_all();
    println!("Stopped.");
    Ok(())
}

fn announce_token(settings: &Settings, token: &str, generated: bool) {
    let line = "=".repeat(78);
    if settings.token.is_some() {
        println!(
            "Sign in with the access token from FOSTA_STUDIO_TOKEN: {}/?token=<token>",
            settings.login_base()
        );
    } else if generated {
        println!("{line}");
        println!("  Fosta Studio is ready. Open this address to sign in (shown only this once):");
        println!();
        println!("    {}/?token={token}", settings.login_base());
        println!();
        println!("  The token is saved in {}.", settings.data_dir.join(auth::TOKEN_FILE).display());
        println!("  Print the address again with: docker exec <container> /app/fosta-studio-server token");
        println!("{line}");
    } else {
        println!(
            "Sign in with the access token saved in {}; print the address with: docker exec <container> /app/fosta-studio-server token",
            settings.data_dir.join(auth::TOKEN_FILE).display()
        );
    }
}

fn warn_auth_off() {
    let line = "!".repeat(78);
    eprintln!("{line}");
    eprintln!("  WARNING: authentication is OFF (FOSTA_STUDIO_AUTH=off).");
    eprintln!("  Anyone who can reach this port can use every saved connection, password and");
    eprintln!("  database through it. Use this only on a trusted machine or behind your own");
    eprintln!("  authenticating proxy. Requests are accepted only for the hosts in");
    eprintln!("  FOSTA_STUDIO_ALLOWED_HOSTS (default: localhost, 127.0.0.1, [::1]).");
    eprintln!("{line}");
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
}
