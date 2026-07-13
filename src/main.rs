use clap::Parser;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use tracing::{error, info, warn};

#[cfg(windows)]
mod win_service;

#[derive(Parser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(long, env = "KINO_RELAY_URL")]
    relay_url: String,

    #[arg(long, env = "KINO_AGENT_ID")]
    agent_id: String,

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

/// Connect, serve, reconnect - until `shutdown` resolves.
async fn run_agent(args: Args, shutdown: impl std::future::Future<Output = ()>) {
    // rustls 0.23 has no implicit default backend; the first wss:// handshake panics
    // unless a provider is installed. Ignore the error - it only means some other
    // component already installed one, which is equally fine.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let control_url = format!("{}/ws/control?agent_id={}", args.relay_url, args.agent_id);
    info!("Connecting to relay control channel at {}", control_url);

    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            _ = &mut shutdown => {
                info!("Shutdown requested; stopping.");
                return;
            }
            _ = control_session(&args, &control_url) => {}
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
async fn control_session(args: &Args, control_url: &str) {
    match connect_async(control_url).await {
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
                                    let relay_url = args.relay_url.clone();
                                    tokio::spawn(async move {
                                        if let Err(e) = handle_new_session(relay_url, session_id).await {
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

async fn handle_new_session(relay_url: String, session_id: String) -> Result<(), Box<dyn std::error::Error>> {
    let data_url = format!("{}/ws/agent/data?session_id={}", relay_url, session_id);
    
    let (ws_stream, _) = connect_async(&data_url).await?;
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
