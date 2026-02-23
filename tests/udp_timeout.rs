//! Integration tests for UDP session timeout and idle detection.
//!
//! These tests spin up a real btest-rs server with short timeouts and
//! verify that UDP sending sessions terminate properly when:
//! - The TCP control channel is closed (client disconnect)
//! - The TCP control channel goes idle (client disappears)
//! - The session deadline is reached
//!
//! Tests run serially to avoid UDP port conflicts.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::{Duration, Instant};

use byteorder::{BigEndian, ByteOrder};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};

/// Monotonically increasing port base to avoid conflicts between tests.
static NEXT_UDP_PORT: AtomicU16 = AtomicU16::new(30_000);

fn alloc_port_range() -> u16 {
    NEXT_UDP_PORT.fetch_add(100, Ordering::SeqCst)
}

/// Build a 16-byte btest command requesting a UDP receive test.
/// (server sends UDP data to client)
fn make_udp_receive_cmd(tx_size: u16) -> [u8; 16] {
    let mut buf = [0u8; 16];
    buf[0] = 0x00; // UDP
    buf[1] = 0x02; // Receive (server sends)
    buf[2] = 0x00; // random data
    buf[3] = 0x00; // single connection
    buf[4] = (tx_size & 0xFF) as u8;
    buf[5] = (tx_size >> 8) as u8;
    buf
}

/// Perform the btest handshake: read hello, send command, read confirm.
/// Returns the allocated UDP port from the server.
async fn handshake(stream: &mut TcpStream, cmd: &[u8; 16]) -> u16 {
    let mut hello = [0u8; 4];
    stream.read_exact(&mut hello).await.unwrap();
    assert_eq!(hello, [0x01, 0x00, 0x00, 0x00], "Expected hello");

    stream.write_all(cmd).await.unwrap();

    let mut confirm = [0u8; 4];
    stream.read_exact(&mut confirm).await.unwrap();
    assert_eq!(confirm[0], 0x01, "Expected confirm");

    let mut port_buf = [0u8; 2];
    stream.read_exact(&mut port_buf).await.unwrap();
    BigEndian::read_u16(&port_buf)
}

/// Start a server and return its listen address.
async fn start_server(
    max_test_duration: Duration,
    control_timeout: Duration,
    udp_port_start: u16,
) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let config = btest_rs::server::ServerConfig {
        listen_addr: addr,
        udp_port_start,
        max_sessions: 10,
        max_test_duration,
        control_timeout,
    };

    tokio::spawn(async move {
        let _ = btest_rs::server::run_server(config).await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    addr
}

/// Wait until no UDP packets are received for `silence_duration`.
/// Returns true if silence was achieved within `deadline`.
async fn wait_for_udp_silence(
    sock: &UdpSocket,
    silence_duration: Duration,
    deadline: Duration,
) -> bool {
    let start = Instant::now();
    let mut buf = [0u8; 2048];

    loop {
        if start.elapsed() > deadline {
            return false;
        }
        match tokio::time::timeout(silence_duration, sock.recv(&mut buf)).await {
            Err(_) => return true, // timeout = silence achieved
            Ok(Ok(_)) => continue, // still receiving, keep draining
            Ok(Err(_)) => return true, // socket error = done
        }
    }
}

/// Test: when the client cleanly closes the TCP control socket,
/// the server's UDP sender should stop promptly.
#[tokio::test]
async fn udp_sender_stops_on_tcp_close() {
    let port_base = alloc_port_range();
    let server_addr = start_server(
        Duration::from_secs(60),
        Duration::from_secs(60),
        port_base,
    )
    .await;

    let cmd = make_udp_receive_cmd(512);
    let mut stream = TcpStream::connect(server_addr).await.unwrap();
    let udp_port = handshake(&mut stream, &cmd).await;

    let client_udp_addr: SocketAddr = format!("127.0.0.1:{}", udp_port + 256).parse().unwrap();
    let client_udp = UdpSocket::bind(client_udp_addr).await.unwrap();

    // Verify we're receiving UDP data
    let mut buf = [0u8; 2048];
    let result = tokio::time::timeout(Duration::from_secs(2), client_udp.recv(&mut buf)).await;
    assert!(result.is_ok(), "Should receive UDP data from server");

    // Close the TCP control socket
    drop(stream);

    // Server detects TCP close → fires shutdown → UDP sender stops.
    // The stats sender writes every 1s; it will get a broken pipe on the
    // next write attempt, which triggers shutdown. So we need to wait
    // at least 1-2 seconds for that cycle, then drain residual packets.
    let silent = wait_for_udp_silence(
        &client_udp,
        Duration::from_secs(2),  // silence window
        Duration::from_secs(10), // overall deadline
    )
    .await;
    assert!(silent, "Server should have stopped sending UDP after TCP close");
}

/// Test: when the client keeps TCP open but sends nothing (idle),
/// the server should terminate the session after control_timeout.
#[tokio::test]
async fn udp_sender_stops_on_control_idle_timeout() {
    let port_base = alloc_port_range();
    let control_timeout = Duration::from_secs(3);
    let server_addr = start_server(
        Duration::from_secs(60),
        control_timeout,
        port_base,
    )
    .await;

    let cmd = make_udp_receive_cmd(512);
    let mut stream = TcpStream::connect(server_addr).await.unwrap();
    let udp_port = handshake(&mut stream, &cmd).await;

    let client_udp_addr: SocketAddr = format!("127.0.0.1:{}", udp_port + 256).parse().unwrap();
    let client_udp = UdpSocket::bind(client_udp_addr).await.unwrap();

    // Verify we're receiving UDP data
    let mut buf = [0u8; 2048];
    let result = tokio::time::timeout(Duration::from_secs(2), client_udp.recv(&mut buf)).await;
    assert!(result.is_ok(), "Should receive UDP data from server");

    // Hold TCP open but send nothing. Server should idle-timeout.
    let start = Instant::now();

    let silent = wait_for_udp_silence(
        &client_udp,
        Duration::from_secs(2),  // silence window
        Duration::from_secs(15), // overall deadline (control_timeout + margin)
    )
    .await;
    let elapsed = start.elapsed();

    assert!(silent, "Server should have stopped sending UDP after idle timeout");
    // Should have taken at least control_timeout
    assert!(
        elapsed >= control_timeout - Duration::from_millis(500),
        "Idle timeout should not fire too early (elapsed: {elapsed:?})"
    );

    drop(stream);
}

/// Test: the session deadline terminates the session even if the client
/// is actively exchanging stats on the control channel.
#[tokio::test]
async fn udp_sender_stops_on_session_deadline() {
    let port_base = alloc_port_range();
    let max_duration = Duration::from_secs(3);
    let server_addr = start_server(
        max_duration,
        Duration::from_secs(60),
        port_base,
    )
    .await;

    let cmd = make_udp_receive_cmd(512);
    let mut stream = TcpStream::connect(server_addr).await.unwrap();
    let udp_port = handshake(&mut stream, &cmd).await;

    let client_udp_addr: SocketAddr = format!("127.0.0.1:{}", udp_port + 256).parse().unwrap();
    let client_udp = UdpSocket::bind(client_udp_addr).await.unwrap();

    // Verify we're receiving UDP data
    let mut buf = [0u8; 2048];
    let result = tokio::time::timeout(Duration::from_secs(2), client_udp.recv(&mut buf)).await;
    assert!(result.is_ok(), "Should receive UDP data from server");

    // Keep the control channel alive by reading server stats
    let (mut reader, _writer) = stream.into_split();
    let keepalive = tokio::spawn(async move {
        let mut buf = [0u8; 1024];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    });

    let start = Instant::now();

    let silent = wait_for_udp_silence(
        &client_udp,
        Duration::from_secs(2),  // silence window
        Duration::from_secs(15), // overall deadline
    )
    .await;
    let elapsed = start.elapsed();

    assert!(silent, "Server should have stopped sending UDP after session deadline");
    assert!(
        elapsed >= max_duration - Duration::from_millis(500),
        "Deadline should not fire too early (elapsed: {elapsed:?})"
    );

    let _ = keepalive.await;
}

/// Test: TCP test also respects the session deadline.
#[tokio::test]
async fn tcp_sender_stops_on_session_deadline() {
    let port_base = alloc_port_range();
    let max_duration = Duration::from_secs(2);
    let server_addr = start_server(
        max_duration,
        Duration::from_secs(60),
        port_base,
    )
    .await;

    let mut cmd = [0u8; 16];
    cmd[0] = 0x01; // TCP
    cmd[1] = 0x02; // Receive (server sends)
    cmd[4] = 0x00;
    cmd[5] = 0x80; // tx_size = 0x8000

    let mut stream = TcpStream::connect(server_addr).await.unwrap();

    let mut hello = [0u8; 4];
    stream.read_exact(&mut hello).await.unwrap();
    assert_eq!(hello, [0x01, 0x00, 0x00, 0x00]);

    stream.write_all(&cmd).await.unwrap();

    let mut confirm = [0u8; 4];
    stream.read_exact(&mut confirm).await.unwrap();

    let start = Instant::now();
    let mut buf = [0u8; 65536];
    let mut total = 0u64;
    loop {
        match tokio::time::timeout(Duration::from_secs(10), stream.read(&mut buf)).await {
            Ok(Ok(0)) => break,
            Ok(Err(_)) => break,
            Ok(Ok(n)) => total += n as u64,
            Err(_) => panic!("TCP stream should have ended by deadline"),
        }
    }
    let elapsed = start.elapsed();

    assert!(total > 0, "Should have received some TCP data");
    assert!(
        elapsed < max_duration + Duration::from_secs(5),
        "TCP session should have been killed by deadline (elapsed: {elapsed:?})"
    );
}
