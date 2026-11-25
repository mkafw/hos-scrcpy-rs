mod device;
mod server;
mod stream;
mod mcp;

use clap::Parser;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::{error, info};

use crate::device::DeviceManager;
use crate::server::{create_router, AppState};
use crate::stream::StreamRelay;

#[derive(Parser, Debug)]
#[command(author, version, about = "HarmonyOS NEXT ultra-low latency screen mirroring & control in pure Rust")]
struct Args {
    #[arg(short, long, default_value = "192.168.1.145:45975")]
    target: String,

    #[arg(short, long, default_value_t = 3000)]
    port: u16,

    #[arg(long, default_value_t = 50051)]
    grpc_port: u16,

    #[arg(long, default_value = "hdc")]
    hdc: String,

    /// Run as MCP (Model Context Protocol) stdio server instead of web UI
    #[arg(long)]
    mcp: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // MCP mode: tracing to stderr, protocol on stdin/stdout
    if args.mcp {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::from_default_env()
                    .add_directive(tracing::Level::INFO.into()),
            )
            .with_writer(std::io::stderr)
            .init();
        return mcp::run_stdio_mcp(args.target, args.hdc).await;
    }

    // Normal web UI mode: tracing to stdout
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    info!("Starting hos-scrcpy-rs...");
    info!("Target device: {}", args.target);
    info!("Web UI Port: {}", args.port);
    info!("gRPC local port: {}", args.grpc_port);

    // 1. Initialize Device Manager & setup daemon
    let device_mgr = Arc::new(DeviceManager::new(
        args.target.clone(),
        args.hdc.clone(),
        args.grpc_port,
    ));

    if let Err(e) = device_mgr.ensure_daemon().await {
        error!("Failed to initialize device daemon: {}", e);
        return Err(e.into());
    }

    // 2. Initialize Stream Relay
    let grpc_url = format!("http://127.0.0.1:{}", args.grpc_port);
    let stream_relay = Arc::new(StreamRelay::new(grpc_url));
    let relay_handle = stream_relay.clone();
    tokio::spawn(async move {
        relay_handle.run_loop().await;
    });

    // 3. Start Web & WebSocket Server
    let state = AppState {
        device: device_mgr,
        relay: stream_relay,
    };

    let app = create_router(state);
    let addr = SocketAddr::from(([0, 0, 0, 0], args.port));
    info!("Server listening on http://127.0.0.1:{}", args.port);
    println!("\n========================================================");
    println!("  鸿蒙纯血极速投屏 (hos-scrcpy-rs) 已启动！");
    println!("  打开浏览器访问: http://127.0.0.1:{}", args.port);
    println!("========================================================\n");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
