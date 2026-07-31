use clap::Parser;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, handshake::client::Request, protocol::Message},
};
use tracing::{error, info, warn};

/// Build the WebSocket request for `url`, attaching `Authorization: Bearer`
/// when a token is configured. A header rather than a query parameter so the
/// secret never lands in relay/proxy access logs.
fn ws_request(
    url: &str,
    token: Option<&str>,
) -> Result<Request, Box<dyn std::error::Error + Send + Sync>> {
    let mut request = url.into_client_request()?;
    if let Some(token) = token.filter(|t| !t.is_empty()) {
        let value = format!("Bearer {token}").parse()?;
        request
            .headers_mut()
            .insert(tokio_tungstenite::tungstenite::http::header::AUTHORIZATION, value);
    }
    Ok(request)
}

#[cfg(windows)]
mod win_service;

#[derive(Parser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Fixed relay to use. Optional when --control-url is set (and then acts
    /// as a fallback if kino-control is unreachable).
    #[arg(long, env = "KINO_RELAY_URL")]
    relay_url: Option<String>,

    /// kino-control base URL (e.g. https://control.example.com). When set,
    /// the agent asks it for the list of enrolled relays, parks on the
    /// fastest healthy one, and reports back where it is. Requires --token.
    #[arg(long, env = "KINO_CONTROL_URL")]
    control_url: Option<String>,

    /// Stable unique id for this host. Optional in Kino Cloud mode
    /// (--agent-key), where kino-control tells us who we are.
    #[arg(long, env = "KINO_AGENT_ID")]
    agent_id: Option<String>,

    /// Bearer token for the relay, when the relay requires auth: either its
    /// static RELAY_TOKEN, or a kino-control token. Ignored when --agent-key
    /// is set (tokens are then fetched and refreshed automatically).
    #[arg(long, env = "KINO_TOKEN")]
    token: Option<String>,

    /// Kino Cloud machine credential ("kca_..."), installed by the one-line
    /// installer. The agent trades it for short-lived relay tokens at startup
    /// and refreshes them in the background. Requires --control-url.
    #[arg(long, env = "KINO_AGENT_KEY")]
    agent_key: Option<String>,

    /// Append logs here instead of stdout. Required when running as a Windows
    /// service, which gives the process no console to write to.
    #[arg(long, env = "KINO_LOG_FILE")]
    log_file: Option<PathBuf>,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type")]
enum ControlMessage {
    #[serde(rename = "new_connection")]
    NewConnection { session_id: String },
}

/// Default to INFO rather than tracing's silent-by-default, so an agent started
/// without RUST_LOG still explains itself.
fn init_logging(log_file: Option<&Path>) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    match log_file {
        Some(path) => {
            if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                let _ = std::fs::create_dir_all(dir);
            }
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap_or_else(|e| panic!("cannot open log file {}: {e}", path.display()));
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_ansi(false)
                .with_writer(file)
                .init();
        }
        None => {
            tracing_subscriber::fmt().with_env_filter(filter).init();
        }
    }
}

/// Credentials that can change while the agent runs: the current bearer token
/// (a static token, or a short-lived JWT the refresher rotates) and the agent
/// id (which kino-control states authoritatively on each refresh).
struct Creds {
    token: std::sync::RwLock<Option<String>>,
    agent_id: std::sync::RwLock<Option<String>>,
}

impl Creds {
    fn token(&self) -> Option<String> {
        self.token.read().expect("creds lock poisoned").clone()
    }
    fn agent_id(&self) -> Option<String> {
        self.agent_id.read().expect("creds lock poisoned").clone()
    }
}

/// One refresh call: trade the agent key for a fresh short-lived relay JWT.
/// Returns its expiry (unix seconds). Blocking - call from spawn_blocking.
fn refresh_token(control_url: &str, agent_key: &str, creds: &Creds) -> Result<u64, String> {
    let response = ureq::post(&format!(
        "{}/api/agents/refresh",
        control_url.trim_end_matches('/')
    ))
    .set("Authorization", &format!("Bearer {agent_key}"))
    .timeout(Duration::from_secs(10))
    .send_string("")
    .map_err(|e| match e {
        ureq::Error::Status(status, resp) => format!(
            "kino-control refused the refresh (HTTP {status}): {}",
            resp.into_string().unwrap_or_default()
        ),
        other => format!("cannot reach kino-control: {other}"),
    })?;
    let json: serde_json::Value = response
        .into_json()
        .map_err(|e| format!("bad refresh response: {e}"))?;
    let token = json["token"].as_str().ok_or("refresh response had no token")?;
    let agent_id = json["agent_id"].as_str().ok_or("refresh response had no agent_id")?;
    let expires_at = json["expires_at"].as_u64().ok_or("refresh response had no expires_at")?;
    *creds.token.write().expect("creds lock poisoned") = Some(token.to_string());
    *creds.agent_id.write().expect("creds lock poisoned") = Some(agent_id.to_string());
    Ok(expires_at)
}

/// wss://relay -> https://relay, ws://relay -> http://relay (for /healthz).
fn ws_to_http(url: &str) -> String {
    if let Some(rest) = url.strip_prefix("wss://") {
        format!("https://{rest}")
    } else if let Some(rest) = url.strip_prefix("ws://") {
        format!("http://{rest}")
    } else {
        url.to_string()
    }
}

/// Ask kino-control for the enrolled relays, race their /healthz endpoints,
/// pick the fastest, and report presence. Blocking (ureq) - call from
/// spawn_blocking. Returns the chosen relay's ws(s):// base URL.
fn discover_relay(control_url: &str, token: &str, agent_id: &str) -> Option<String> {
    let control_url = control_url.trim_end_matches('/');
    let relays: serde_json::Value = ureq::get(&format!("{control_url}/api/relays"))
        .set("Authorization", &format!("Bearer {token}"))
        .timeout(Duration::from_secs(5))
        .call()
        .map_err(|e| warn!("kino-control unreachable: {e}"))
        .ok()?
        .into_json()
        .ok()?;

    let mut healthy: Vec<String> = relays
        .as_array()?
        .iter()
        .filter(|r| r["healthy"].as_bool() == Some(true))
        .filter_map(|r| r["url"].as_str().map(str::to_string))
        .collect();
    if healthy.is_empty() {
        warn!("kino-control reports no healthy relays");
        return None;
    }
    // The list arrives policy-ordered (best candidates first); race only the
    // top few rather than probing an entire large fleet.
    healthy.truncate(5);

    // Latency race: probe each relay and take the quickest to answer. A probe
    // measures the whole TLS+HTTP round trip, which is what a session feels.
    let mut best: Option<(u128, String)> = None;
    for url in healthy {
        let probe = format!("{}/healthz", ws_to_http(&url).trim_end_matches('/'));
        let started = std::time::Instant::now();
        match ureq::get(&probe).timeout(Duration::from_secs(3)).call() {
            Ok(_) => {
                let ms = started.elapsed().as_millis();
                info!("Relay {url}: {ms}ms");
                if best.as_ref().map(|(b, _)| ms < *b).unwrap_or(true) {
                    best = Some((ms, url));
                }
            }
            Err(e) => warn!("Relay {url} probe failed: {e}"),
        }
    }
    let (ms, chosen) = best?;
    info!("Parking on {chosen} ({ms}ms)");

    // Tell kino-control where we are, so managers can find us. Best-effort:
    // a failed report only means discovery-based managers can't locate us yet.
    if let Err(e) = ureq::post(&format!("{control_url}/api/agents/presence"))
        .set("Authorization", &format!("Bearer {token}"))
        .timeout(Duration::from_secs(5))
        .send_json(serde_json::json!({ "relay_url": chosen, "agent_id": agent_id }))
    {
        warn!("Failed to report presence to kino-control: {e}");
    }
    Some(chosen)
}

fn main() {
    // Under the Windows SCM this hands off to the service dispatcher and blocks
    // until the service stops. Run from a console it fails immediately with
    // ERROR_FAILED_SERVICE_CONTROLLER_CONNECT, and we fall through to foreground.
    #[cfg(windows)]
    if win_service::try_run_as_service() {
        return;
    }

    let args = Args::parse();
    init_logging(args.log_file.as_deref());

    if args.relay_url.is_none() && args.control_url.is_none() {
        eprintln!("error: pass --relay-url, --control-url, or both (control mode discovers relays; relay-url is the fallback)");
        std::process::exit(2);
    }
    if args.agent_key.is_some() && args.control_url.is_none() {
        eprintln!("error: --agent-key requires --control-url (it refreshes tokens from kino-control)");
        std::process::exit(2);
    }
    if args.control_url.is_some() && args.token.is_none() && args.agent_key.is_none() {
        eprintln!("error: --control-url requires --token or --agent-key");
        std::process::exit(2);
    }
    if args.agent_id.is_none() && args.agent_key.is_none() {
        eprintln!("error: --agent-id is required (it's optional only with --agent-key, where kino-control assigns it)");
        std::process::exit(2);
    }

    let runtime = tokio::runtime::Runtime::new().expect("failed to start tokio runtime");
    runtime.block_on(async move {
        run_agent(args, shutdown_signal()).await;
    });
}

/// Resolves when the OS asks us to stop: Ctrl-C anywhere, or SIGTERM on Unix
/// (which is what systemd and `docker stop` send).
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        if let Ok(mut sigterm) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {},
                _ = sigterm.recv() => {},
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

/// Pick which relay to use for the next connect cycle: discovery via
/// kino-control when configured (falling back to --relay-url if discovery
/// fails), otherwise the fixed --relay-url.
async fn pick_relay(args: &Args, creds: &std::sync::Arc<Creds>) -> Option<String> {
    if let Some(control_url) = args.control_url.clone() {
        if let (Some(token), Some(agent_id)) = (creds.token(), creds.agent_id()) {
            let discovered = tokio::task::spawn_blocking(move || {
                discover_relay(&control_url, &token, &agent_id)
            })
            .await
            .ok()
            .flatten();
            if discovered.is_some() {
                return discovered;
            }
        }
        if args.relay_url.is_some() {
            warn!("Discovery failed; falling back to --relay-url");
        }
    }
    args.relay_url.clone()
}

/// Connect, serve, reconnect - until `shutdown` resolves. Each cycle re-picks
/// the relay, so an agent in control mode migrates automatically when its
/// relay dies or a faster one appears.
async fn run_agent(args: Args, shutdown: impl std::future::Future<Output = ()>) {
    // rustls 0.23 has no implicit default backend; the first wss:// handshake panics
    // unless a provider is installed. Ignore the error - it only means some other
    // component already installed one, which is equally fine.
    let _ = rustls::crypto::ring::default_provider().install_default();

    tokio::pin!(shutdown);

    let creds = std::sync::Arc::new(Creds {
        token: std::sync::RwLock::new(args.token.clone()),
        agent_id: std::sync::RwLock::new(args.agent_id.clone()),
    });

    // Kino Cloud mode: obtain the first short-lived token before anything
    // else (retrying - the machine may boot before its network), then keep it
    // fresh in the background for as long as we run.
    if let Some(agent_key) = args.agent_key.clone() {
        let control_url = args.control_url.clone().expect("checked in main");
        let mut expires_at;
        loop {
            let (c, ctl, key) = (creds.clone(), control_url.clone(), agent_key.clone());
            match tokio::task::spawn_blocking(move || refresh_token(&ctl, &key, &c)).await {
                Ok(Ok(exp)) => {
                    info!("Obtained agent token from kino-control");
                    expires_at = exp;
                    break;
                }
                Ok(Err(e)) => warn!("Token refresh failed: {e}; retrying in 30s"),
                Err(e) => warn!("Token refresh task failed: {e}; retrying in 30s"),
            }
            tokio::select! {
                _ = &mut shutdown => { info!("Shutdown requested; stopping."); return; }
                _ = tokio::time::sleep(Duration::from_secs(30)) => {}
            }
        }

        let refresher_creds = creds.clone();
        tokio::spawn(async move {
            loop {
                // Refresh at ~3/4 of the token's remaining life, never more
                // often than once a minute.
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let wait = (expires_at.saturating_sub(now) * 3 / 4).max(60);
                tokio::time::sleep(Duration::from_secs(wait)).await;
                loop {
                    let (c, ctl, key) =
                        (refresher_creds.clone(), control_url.clone(), agent_key.clone());
                    match tokio::task::spawn_blocking(move || refresh_token(&ctl, &key, &c)).await
                    {
                        Ok(Ok(exp)) => {
                            info!("Refreshed agent token");
                            expires_at = exp;
                            break;
                        }
                        // Keep trying on a short cadence: refresh failure is
                        // either an outage (transient) or the machine was
                        // removed / subscription lapsed (fails forever, and
                        // the relay starts rejecting us when the token dies).
                        Ok(Err(e)) => warn!("Token refresh failed: {e}; retrying in 5m"),
                        Err(e) => warn!("Token refresh task failed: {e}; retrying in 5m"),
                    }
                    tokio::time::sleep(Duration::from_secs(300)).await;
                }
            }
        });
    }

    loop {
        let cycle = async {
            match pick_relay(&args, &creds).await {
                Some(relay_base) => {
                    let relay_base = relay_base.trim_end_matches('/').to_string();
                    let Some(agent_id) = creds.agent_id() else {
                        warn!("No agent id available yet; retrying shortly");
                        return;
                    };
                    let control_url =
                        format!("{}/ws/control?agent_id={}", relay_base, agent_id);
                    info!("Connecting to relay control channel at {}", control_url);
                    control_session(&creds, &relay_base, &control_url).await;
                }
                None => warn!("No relay available; retrying shortly"),
            }
        };
        tokio::select! {
            _ = &mut shutdown => {
                info!("Shutdown requested; stopping.");
                return;
            }
            _ = cycle => {}
        }

        // Back off before retrying, but stay responsive to a stop request.
        tokio::select! {
            _ = &mut shutdown => {
                info!("Shutdown requested; stopping.");
                return;
            }
            _ = tokio::time::sleep(Duration::from_secs(5)) => {}
        }
    }
}

/// One connect-and-serve cycle on the control channel. Returns when it drops.
/// `relay_base` is the ws(s):// base data sockets should dial for sessions.
async fn control_session(creds: &Creds, relay_base: &str, control_url: &str) {
    let request = match ws_request(control_url, creds.token().as_deref()) {
        Ok(r) => r,
        Err(e) => {
            error!("Invalid relay URL or token: {}", e);
            return;
        }
    };
    match connect_async(request).await {
        Ok((ws_stream, _)) => {
            info!("Connected to relay control channel.");
            let (mut ws_tx, mut rx) = ws_stream.split();

            // A single writer owns the sink; the reader and the ping ticker
            // feed it over this channel. This lets us reply to the relay's
            // pings (keeping the connection alive through NATs) and send our
            // own, so a silently-dropped connection surfaces as a write error
            // and drives us back into the reconnect loop below.
            let (out_tx, mut out_rx) = mpsc::channel::<Message>(16);
            let writer = tokio::spawn(async move {
                while let Some(m) = out_rx.recv().await {
                    if ws_tx.send(m).await.is_err() {
                        break;
                    }
                }
            });

            let ping_tx = out_tx.clone();
            let pinger = tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(30));
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                interval.tick().await; // consume the immediate first tick
                loop {
                    interval.tick().await;
                    if ping_tx.send(Message::Ping(Vec::new())).await.is_err() {
                        break;
                    }
                }
            });

            while let Some(msg) = rx.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        if let Ok(cmd) = serde_json::from_str::<ControlMessage>(text.to_string().as_str()) {
                            match cmd {
                                ControlMessage::NewConnection { session_id } => {
                                    info!("Received new connection request for session {}", session_id);
                                    let relay_url = relay_base.to_string();
                                    // Snapshot per session, so refreshed
                                    // tokens are picked up immediately.
                                    let token = creds.token();
                                    tokio::spawn(async move {
                                        if let Err(e) = handle_new_session(relay_url, token, session_id).await {
                                            error!("Session error: {}", e);
                                        }
                                    });
                                }
                            }
                        }
                    }
                    Ok(Message::Ping(payload)) => {
                        if out_tx.send(Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Ok(Message::Close(_)) => {
                        warn!("Control channel closed by server.");
                        break;
                    }
                    Err(e) => {
                        error!("Control channel error: {}", e);
                        break;
                    }
                    _ => {}
                }
            }

            // Tear down the writer/pinger before reconnecting.
            drop(out_tx);
            pinger.abort();
            writer.abort();
        }
        Err(e) => {
            error!("Failed to connect to relay: {}. Retrying in 5 seconds...", e);
        }
    }
}

async fn handle_new_session(
    relay_url: String,
    token: Option<String>,
    session_id: String,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let data_url = format!("{}/ws/agent/data?session_id={}", relay_url, session_id);

    let (ws_stream, _) = connect_async(ws_request(&data_url, token.as_deref())?).await?;
    info!("Session {}: Connected to relay data channel", session_id);
    
    let tcp_stream = TcpStream::connect("127.0.0.1:22").await?;
    info!("Session {}: Connected to local SSH daemon", session_id);
    
    let (mut ws_tx, mut ws_rx) = ws_stream.split();
    let (mut tcp_rx, mut tcp_tx) = tokio::io::split(tcp_stream);
    
    let session_id_clone = session_id.clone();
    let tcp_to_ws = tokio::spawn(async move {
        let mut buf = [0u8; 8192];
        use tokio::io::AsyncReadExt;
        loop {
            match tcp_rx.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    if ws_tx.send(Message::Binary(buf[..n].into())).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        info!("Session {}: TCP->WS closed", session_id_clone);
    });
    
    let session_id_clone = session_id.clone();
    let ws_to_tcp = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        while let Some(msg) = ws_rx.next().await {
            match msg {
                Ok(Message::Binary(data)) => {
                    if tcp_tx.write_all(&data).await.is_err() {
                        break;
                    }
                }
                Ok(Message::Close(_)) => break,
                Err(_) => break,
                _ => {}
            }
        }
        info!("Session {}: WS->TCP closed", session_id_clone);
    });
    
    tokio::select! {
        _ = tcp_to_ws => {},
        _ = ws_to_tcp => {},
    }
    
    info!("Session {} closed", session_id);
    Ok(())
}
