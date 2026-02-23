use std::net::SocketAddr;
use std::time::Duration;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use btest_rs::protocol::BTEST_PORT;
use btest_rs::server::{run_server, ServerConfig};

#[derive(Parser, Debug)]
#[command(
    name = "btest-rs",
    version,
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

    /// Maximum test duration in seconds (hard cap per session)
    #[arg(long, default_value_t = 300)]
    max_test_duration: u64,

    /// Idle timeout in seconds for the TCP control channel.
    /// If no data is received from the client within this period,
    /// the session is terminated. Prevents zombie UDP streams.
    #[arg(long, default_value_t = 30)]
    control_timeout: u64,
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
        max_test_duration: Duration::from_secs(args.max_test_duration),
        control_timeout: Duration::from_secs(args.control_timeout),
    };

    run_server(config).await
}
