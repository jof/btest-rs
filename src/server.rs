use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::Notify;
use tokio::time;
use tracing::{debug, info, warn};

use crate::protocol::*;

/// Server configuration
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub listen_addr: SocketAddr,
    pub udp_port_start: u16,
    pub max_sessions: u32,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen_addr: SocketAddr::from(([0, 0, 0, 0], BTEST_PORT)),
            udp_port_start: 2000,
            max_sessions: 100,
        }
    }
}

/// Shared state for UDP port allocation
struct ServerState {
    next_udp_port: AtomicU32,
    active_sessions: AtomicU32,
    config: ServerConfig,
}

impl ServerState {
    fn new(config: ServerConfig) -> Self {
        Self {
            next_udp_port: AtomicU32::new(config.udp_port_start as u32),
            active_sessions: AtomicU32::new(0),
            config,
        }
    }

    fn allocate_udp_port(&self) -> u16 {
        let port = self.next_udp_port.fetch_add(1, Ordering::Relaxed);
        // Wrap around if we go too high
        if port > 65000 {
            self.next_udp_port
                .store(self.config.udp_port_start as u32 + 1, Ordering::Relaxed);
        }
        port as u16
    }
}

pub async fn run_server(config: ServerConfig) -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(config.listen_addr).await?;
    info!("BTest server listening on {}", config.listen_addr);

    let state = Arc::new(ServerState::new(config));

    loop {
        let (stream, peer) = listener.accept().await?;
        let state = Arc::clone(&state);

        let active = state.active_sessions.fetch_add(1, Ordering::Relaxed);
        if active >= state.config.max_sessions {
            state.active_sessions.fetch_sub(1, Ordering::Relaxed);
            warn!(%peer, "Rejecting connection: max sessions reached");
            continue;
        }

        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, peer, &state).await {
                debug!(%peer, error = %e, "Connection ended");
            }
            state.active_sessions.fetch_sub(1, Ordering::Relaxed);
        });
    }
}

async fn handle_connection(
    mut stream: TcpStream,
    peer: SocketAddr,
    state: &ServerState,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    stream.set_nodelay(true)?;
    info!(%peer, "New connection");

    // Step 1: Send hello
    stream.write_all(&MSG_HELLO).await?;
    debug!(%peer, "Sent hello");

    // Step 2: Receive 16-byte command
    let mut cmd_buf = [0u8; 16];
    stream.read_exact(&mut cmd_buf).await?;

    let cmd = Command::parse(&cmd_buf).ok_or("Invalid command")?;
    info!(%peer, %cmd, "Received command");

    // Step 3: Send confirm (no auth)
    let confirm = if cmd.tcp_conn_count > 0 {
        ConfirmMulti::new(1) // Simple session ID for now
    } else {
        ConfirmMulti::single()
    };
    stream.write_all(&confirm.token).await?;
    debug!(%peer, "Sent confirm");

    // Step 4: Run the test based on protocol
    match cmd.protocol {
        Protocol::Tcp => handle_tcp_test(stream, peer, &cmd).await,
        Protocol::Udp => handle_udp_test(stream, peer, &cmd, state).await,
    }
}

async fn handle_tcp_test(
    stream: TcpStream,
    peer: SocketAddr,
    cmd: &Command,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    info!(%peer, "Starting TCP bandwidth test");

    let (mut reader, mut writer) = stream.into_split();
    let recv_bytes = Arc::new(AtomicU64::new(0));
    let shutdown = Arc::new(Notify::new());

    // Determine what we need to do based on direction
    // direction is from the CLIENT's perspective:
    //   Transmit = client sends, server receives
    //   Receive  = client receives, server sends
    //   Both     = bidirectional
    let server_should_send = matches!(cmd.direction, Direction::Receive | Direction::Both);

    let tx_size = cmd.tx_size as usize;
    let recv_bytes_tx = Arc::clone(&recv_bytes);
    let shutdown_rx = Arc::clone(&shutdown);
    let shutdown_tx = Arc::clone(&shutdown);

    // Receiver task: reads data from client, counts bytes, watches for stats
    let recv_handle = tokio::spawn(async move {
        let mut buf = vec![0u8; 65536];
        loop {
            tokio::select! {
                result = reader.read(&mut buf) => {
                    match result {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            recv_bytes_tx.fetch_add(n as u64, Ordering::Relaxed);
                        }
                    }
                }
                _ = shutdown_rx.notified() => break,
            }
        }
    });

    // Sender task: sends data to client and periodic stats
    let send_handle = tokio::spawn(async move {
        let mut seq: u32 = 0;
        let mut stat_interval = time::interval(Duration::from_secs(1));
        stat_interval.tick().await; // skip first immediate tick

        // Pre-allocate send buffer (zeros, with 0x07 marker at start)
        let mut data_buf = vec![0u8; tx_size.max(1460)];
        data_buf[0] = 0x00; // data marker

        loop {
            tokio::select! {
                _ = stat_interval.tick() => {
                    seq += 1;
                    let bytes = recv_bytes.swap(0, Ordering::Relaxed) as u32;
                    let stat = Stats { seq, recv_bytes: bytes };
                    let stat_buf = stat.serialize();
                    if writer.write_all(&stat_buf).await.is_err() {
                        break;
                    }
                    debug!(%peer, seq, bytes, "Sent stats");
                }
                _ = shutdown_tx.notified() => break,
                _ = async {
                    if server_should_send {
                        // Send data as fast as possible
                        if writer.write_all(&data_buf[..tx_size.max(1)]).await.is_err() {
                            shutdown_tx.notify_waiters();
                        }
                    } else {
                        // If we're only receiving, just wait for stats interval
                        tokio::time::sleep(Duration::from_secs(3600)).await;
                    }
                } => {}
            }
        }
    });

    let _ = tokio::join!(recv_handle, send_handle);
    info!(%peer, "TCP test complete");
    Ok(())
}

async fn handle_udp_test(
    mut stream: TcpStream,
    peer: SocketAddr,
    cmd: &Command,
    state: &ServerState,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Allocate a UDP port and send it to the client (2 bytes, big-endian)
    let udp_port = state.allocate_udp_port();
    let port_buf = [(udp_port >> 8) as u8, (udp_port & 0xFF) as u8];
    stream.write_all(&port_buf).await?;
    info!(%peer, udp_port, "Starting UDP bandwidth test");

    // Bind the UDP socket
    let udp_addr = SocketAddr::from(([0, 0, 0, 0], udp_port));
    let udp_socket = UdpSocket::bind(udp_addr).await?;

    let server_should_send = matches!(cmd.direction, Direction::Receive | Direction::Both);
    let tx_size = cmd.tx_size as usize;

    let recv_bytes = Arc::new(AtomicU64::new(0));
    let shutdown = Arc::new(Notify::new());

    let udp_socket = Arc::new(udp_socket);

    // UDP receiver task
    let udp_recv = Arc::clone(&udp_socket);
    let recv_bytes_udp = Arc::clone(&recv_bytes);
    let shutdown_udp_rx = Arc::clone(&shutdown);
    let recv_handle = tokio::spawn(async move {
        let mut buf = vec![0u8; 65536];
        let mut client_addr: Option<SocketAddr> = None;
        loop {
            tokio::select! {
                result = udp_recv.recv_from(&mut buf) => {
                    match result {
                        Ok((n, addr)) => {
                            if client_addr.is_none() {
                                client_addr = Some(addr);
                                debug!(%addr, "UDP client connected");
                            }
                            // Count bytes including IP+UDP header overhead (28 bytes)
                            recv_bytes_udp.fetch_add((n + 28) as u64, Ordering::Relaxed);
                        }
                        Err(e) => {
                            debug!(error = %e, "UDP recv error");
                            break;
                        }
                    }
                }
                _ = shutdown_udp_rx.notified() => break,
            }
        }
    });

    // UDP sender task
    let udp_send = Arc::clone(&udp_socket);
    let shutdown_udp_tx = Arc::clone(&shutdown);
    let client_ip = peer.ip();
    // MikroTik client listens on udp_port + 256 (BTEST_PORT_CLIENT_OFFSET)
    let client_udp_port = udp_port + BTEST_PORT_CLIENT_OFFSET;
    let send_handle = if server_should_send {
        Some(tokio::spawn(async move {
            // Wait briefly for the client to be ready
            time::sleep(Duration::from_millis(100)).await;

            let client_addr = SocketAddr::new(client_ip, client_udp_port);
            let mut buf = vec![0u8; tx_size.saturating_sub(28).max(4)];
            let mut seq: u32 = 1;

            loop {
                // Write sequence number (big-endian) in first 4 bytes
                buf[0] = (seq >> 24) as u8;
                buf[1] = (seq >> 16) as u8;
                buf[2] = (seq >> 8) as u8;
                buf[3] = seq as u8;
                seq = seq.wrapping_add(1);

                tokio::select! {
                    result = udp_send.send_to(&buf, client_addr) => {
                        if result.is_err() {
                            break;
                        }
                    }
                    _ = shutdown_udp_tx.notified() => break,
                }
                // Small yield to prevent busy-spinning at low speeds
                tokio::task::yield_now().await;
            }
        }))
    } else {
        None
    };

    // Control channel: exchange stats every second, watch for disconnect
    let mut seq: u32 = 0;
    let mut stat_interval = time::interval(Duration::from_secs(1));
    stat_interval.tick().await;

    let mut ctrl_buf = [0u8; 1024];
    let (mut ctrl_reader, mut ctrl_writer) = stream.into_split();

    // Stats sender on control channel
    let recv_bytes_ctrl = Arc::clone(&recv_bytes);
    let shutdown_ctrl = Arc::clone(&shutdown);
    let ctrl_send_handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = stat_interval.tick() => {
                    seq += 1;
                    let bytes = recv_bytes_ctrl.swap(0, Ordering::Relaxed) as u32;
                    let stat = Stats { seq, recv_bytes: bytes };
                    let stat_bytes = stat.serialize();
                    if ctrl_writer.write_all(&stat_bytes).await.is_err() {
                        shutdown_ctrl.notify_waiters();
                        break;
                    }
                }
                _ = shutdown_ctrl.notified() => break,
            }
        }
    });

    // Control channel reader (receives stats from client, detects disconnect)
    let shutdown_ctrl_rx = Arc::clone(&shutdown);
    let ctrl_recv_handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                result = ctrl_reader.read(&mut ctrl_buf) => {
                    match result {
                        Ok(0) | Err(_) => {
                            shutdown_ctrl_rx.notify_waiters();
                            break;
                        }
                        Ok(n) => {
                            // Parse incoming stats from client (12-byte messages starting with 0x07)
                            if n >= 12 && ctrl_buf[0] == MSG_STAT {
                                if let Some(remote_stat) = Stats::parse(ctrl_buf[..12].try_into().unwrap()) {
                                    debug!(seq = remote_stat.seq, bytes = remote_stat.recv_bytes, "Remote stats");
                                }
                            }
                        }
                    }
                }
                _ = shutdown_ctrl_rx.notified() => break,
            }
        }
    });

    // Wait for everything to finish
    let _ = tokio::join!(recv_handle, ctrl_send_handle, ctrl_recv_handle);
    if let Some(h) = send_handle {
        let _ = h.await;
    }

    info!(%peer, "UDP test complete");
    Ok(())
}
