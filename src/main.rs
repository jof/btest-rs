mod protocol;
mod server;

use clap::Parser;
use std::net::SocketAddr;
use tracing_subscriber::EnvFilter;

use crate::protocol::BTEST_PORT;
use crate::server::{run_server, ServerConfig};

#[derive(Parser, Debug)]
#[command(
    name = "btest-rs",
    about = "High-performance MikroTik bandwidth-test protocol server"
)]
struct Args {
    /// Listen address
    #[arg(short, long, default_value_t = format!("0.0.0.0:{}", BTEST_PORT))]
    listen: String,

    /// Starting UDP port for allocation
    #[arg(long, default_value_t = 2000)]
    udp_port_start: u16,

    /// Maximum concurrent sessions
    #[arg(long, default_value_t = 100)]
    max_sessions: u32,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    let listen_addr: SocketAddr = args.listen.parse()?;

    let config = ServerConfig {
        listen_addr,
        udp_port_start: args.udp_port_start,
        max_sessions: args.max_sessions,
    };

    run_server(config).await
}
