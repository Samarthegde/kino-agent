use clap::Parser;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use tracing::{info, warn, error};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(long, env = "KINO_RELAY_URL")]
    relay_url: String,

    #[arg(long, env = "KINO_AGENT_ID")]
    agent_id: String,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type")]
enum ControlMessage {
    #[serde(rename = "new_connection")]
    NewConnection { session_id: String },
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    let control_url = format!("{}/ws/control?agent_id={}", args.relay_url, args.agent_id);
    info!("Connecting to relay control channel at {}", control_url);

    loop {
        match connect_async(&control_url).await {
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
        
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
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
