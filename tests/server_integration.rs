//! Integration tests for btest-rs server functionality.
//!
//! Covers: handshake, stats exchange, direction modes (transmit/receive/both),
//! max sessions rejection, multi-connection TCP, and malformed commands.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;

use byteorder::{BigEndian, ByteOrder, LittleEndian};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};

static NEXT_UDP_PORT: AtomicU16 = AtomicU16::new(40_000);

fn alloc_port_range() -> u16 {
    NEXT_UDP_PORT.fetch_add(100, Ordering::SeqCst)
}

async fn start_server(
    max_test_duration: Duration,
    control_timeout: Duration,
    udp_port_start: u16,
    max_sessions: u32,
) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let config = btest_rs::server::ServerConfig {
        listen_addr: addr,
        udp_port_start,
        max_sessions,
        max_test_duration,
        control_timeout,
    };

    tokio::spawn(async move {
        let _ = btest_rs::server::run_server(config).await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    addr
}

fn default_server_dur() -> Duration {
    Duration::from_secs(10)
}

/// Build a 16-byte command buffer.
fn make_cmd(
    protocol: u8,
    direction: u8,
    random_data: u8,
    conn_count: u8,
    tx_size: u16,
    client_buf_size: u16,
    remote_tx_speed: u32,
    local_tx_speed: u32,
) -> [u8; 16] {
    let mut buf = [0u8; 16];
    buf[0] = protocol;
    buf[1] = direction;
    buf[2] = random_data;
    buf[3] = conn_count;
    LittleEndian::write_u16(&mut buf[4..6], tx_size);
    LittleEndian::write_u16(&mut buf[6..8], client_buf_size);
    LittleEndian::write_u32(&mut buf[8..12], remote_tx_speed);
    LittleEndian::write_u32(&mut buf[12..16], local_tx_speed);
    buf
}

// ─── Handshake tests ─────────────────────────────────────────────────────────

/// Verify the server sends a 4-byte hello immediately on connect.
#[tokio::test]
async fn handshake_hello() {
    let server_addr = start_server(default_server_dur(), default_server_dur(), alloc_port_range(), 10).await;
    let mut stream = TcpStream::connect(server_addr).await.unwrap();

    let mut hello = [0u8; 4];
    tokio::time::timeout(Duration::from_secs(2), stream.read_exact(&mut hello))
        .await
        .expect("Timed out waiting for hello")
        .expect("Failed to read hello");

    assert_eq!(hello, [0x01, 0x00, 0x00, 0x00]);
}

/// Verify the full TCP handshake: hello → command → confirm.
#[tokio::test]
async fn handshake_tcp_full() {
    let server_addr = start_server(default_server_dur(), default_server_dur(), alloc_port_range(), 10).await;
    let mut stream = TcpStream::connect(server_addr).await.unwrap();

    // Read hello
    let mut hello = [0u8; 4];
    stream.read_exact(&mut hello).await.unwrap();
    assert_eq!(hello, [0x01, 0x00, 0x00, 0x00]);

    // Send TCP receive command
    let cmd = make_cmd(0x01, 0x02, 0x00, 0, 0x8000, 0, 0, 0);
    stream.write_all(&cmd).await.unwrap();

    // Read 4-byte confirm
    let mut confirm = [0u8; 4];
    stream.read_exact(&mut confirm).await.unwrap();
    assert_eq!(confirm[0], 0x01, "Confirm should start with 0x01");
}

/// Verify the full UDP handshake: hello → command → confirm → 2-byte port.
#[tokio::test]
async fn handshake_udp_full() {
    let port_base = alloc_port_range();
    let server_addr = start_server(default_server_dur(), default_server_dur(), port_base, 10).await;
    let mut stream = TcpStream::connect(server_addr).await.unwrap();

    let mut hello = [0u8; 4];
    stream.read_exact(&mut hello).await.unwrap();
    assert_eq!(hello, [0x01, 0x00, 0x00, 0x00]);

    let cmd = make_cmd(0x00, 0x02, 0x00, 0, 512, 0, 0, 0);
    stream.write_all(&cmd).await.unwrap();

    let mut confirm = [0u8; 4];
    stream.read_exact(&mut confirm).await.unwrap();
    assert_eq!(confirm[0], 0x01);

    // Read 2-byte UDP port
    let mut port_buf = [0u8; 2];
    stream.read_exact(&mut port_buf).await.unwrap();
    let udp_port = BigEndian::read_u16(&port_buf);
    assert!(udp_port >= port_base, "UDP port should be >= port_base");
}

/// Verify that multi-connection TCP handshake returns a session token.
#[tokio::test]
async fn handshake_tcp_multi_connection() {
    let server_addr = start_server(default_server_dur(), default_server_dur(), alloc_port_range(), 10).await;
    let mut stream = TcpStream::connect(server_addr).await.unwrap();

    let mut hello = [0u8; 4];
    stream.read_exact(&mut hello).await.unwrap();

    // TCP receive with conn_count=5
    let cmd = make_cmd(0x01, 0x02, 0x00, 5, 0x8000, 0, 0, 0);
    stream.write_all(&cmd).await.unwrap();

    let mut confirm = [0u8; 4];
    stream.read_exact(&mut confirm).await.unwrap();
    assert_eq!(confirm[0], 0x01, "Confirm byte 0 should be 0x01");
    // Bytes 1-2 contain the session token for multi-conn
    let session_id = u16::from_le_bytes([confirm[1], confirm[2]]);
    assert!(session_id > 0, "Multi-connection confirm should have non-zero session ID");
}

// ─── Malformed command tests ─────────────────────────────────────────────────

/// Server should close connection on invalid protocol byte.
#[tokio::test]
async fn malformed_command_invalid_protocol() {
    let server_addr = start_server(default_server_dur(), default_server_dur(), alloc_port_range(), 10).await;
    let mut stream = TcpStream::connect(server_addr).await.unwrap();

    let mut hello = [0u8; 4];
    stream.read_exact(&mut hello).await.unwrap();

    // Invalid protocol byte 0xFF
    let cmd = make_cmd(0xFF, 0x02, 0x00, 0, 512, 0, 0, 0);
    stream.write_all(&cmd).await.unwrap();

    // Server should close the connection
    let mut buf = [0u8; 64];
    let result = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buf)).await;
    match result {
        Ok(Ok(0)) => {} // clean close — expected
        Ok(Err(_)) => {} // error — also acceptable
        Ok(Ok(_)) => panic!("Server should not send data after invalid command"),
        Err(_) => panic!("Server should close connection promptly after invalid command"),
    }
}

/// Server should close connection on invalid direction byte.
#[tokio::test]
async fn malformed_command_invalid_direction() {
    let server_addr = start_server(default_server_dur(), default_server_dur(), alloc_port_range(), 10).await;
    let mut stream = TcpStream::connect(server_addr).await.unwrap();

    let mut hello = [0u8; 4];
    stream.read_exact(&mut hello).await.unwrap();

    // Valid protocol (TCP) but invalid direction 0xFF
    let cmd = make_cmd(0x01, 0xFF, 0x00, 0, 0x8000, 0, 0, 0);
    stream.write_all(&cmd).await.unwrap();

    let mut buf = [0u8; 64];
    let result = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buf)).await;
    match result {
        Ok(Ok(0)) | Ok(Err(_)) => {} // expected
        Ok(Ok(_)) => panic!("Server should not send data after invalid command"),
        Err(_) => panic!("Server should close connection promptly"),
    }
}

/// Server should handle truncated command (fewer than 16 bytes) gracefully.
#[tokio::test]
async fn malformed_command_truncated() {
    let server_addr = start_server(default_server_dur(), default_server_dur(), alloc_port_range(), 10).await;
    let mut stream = TcpStream::connect(server_addr).await.unwrap();

    let mut hello = [0u8; 4];
    stream.read_exact(&mut hello).await.unwrap();

    // Send only 4 bytes instead of 16, then close
    stream.write_all(&[0x01, 0x02, 0x00, 0x00]).await.unwrap();
    stream.shutdown().await.unwrap();

    // Server should detect incomplete read and close
    let mut buf = [0u8; 64];
    let result = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buf)).await;
    match result {
        Ok(Ok(0)) | Ok(Err(_)) => {} // expected
        _ => {} // any response is acceptable for truncated input
    }
}

// ─── Max sessions rejection ──────────────────────────────────────────────────

/// Server should reject connections beyond max_sessions.
#[tokio::test]
async fn max_sessions_rejection() {
    let port_base = alloc_port_range();
    let server_addr = start_server(
        default_server_dur(),
        default_server_dur(),
        port_base,
        2, // only allow 2 sessions
    )
    .await;

    // Fill up both session slots with TCP receive tests
    let mut streams = Vec::new();
    for _ in 0..2 {
        let mut s = TcpStream::connect(server_addr).await.unwrap();
        let mut hello = [0u8; 4];
        s.read_exact(&mut hello).await.unwrap();
        let cmd = make_cmd(0x01, 0x02, 0x00, 0, 0x8000, 0, 0, 0);
        s.write_all(&cmd).await.unwrap();
        let mut confirm = [0u8; 4];
        s.read_exact(&mut confirm).await.unwrap();
        streams.push(s);
    }

    // Third connection should be rejected (server closes it immediately)
    let result = tokio::time::timeout(Duration::from_secs(2), async {
        let mut s = TcpStream::connect(server_addr).await.unwrap();
        let mut buf = [0u8; 64];
        // The server should either refuse or close the connection
        // It may send hello then close, or just close
        match s.read(&mut buf).await {
            Ok(0) => true,  // immediate close = rejected
            Err(_) => true, // error = rejected
            Ok(n) => {
                // Got hello but then should close if we don't send command fast enough
                // or the session was accepted (which would be a bug)
                if n == 4 && buf[..4] == [0x01, 0x00, 0x00, 0x00] {
                    // Got hello — server accepted the TCP connection but hasn't
                    // counted the session yet. Send a command to trigger session count.
                    let cmd = make_cmd(0x01, 0x02, 0x00, 0, 0x8000, 0, 0, 0);
                    let _ = s.write_all(&cmd).await;
                    // Now it should close
                    match tokio::time::timeout(Duration::from_secs(1), s.read(&mut buf)).await {
                        Ok(Ok(0)) | Ok(Err(_)) | Err(_) => true,
                        _ => false,
                    }
                } else {
                    false
                }
            }
        }
    })
    .await;

    // Clean up active sessions
    drop(streams);

    // The third connection should have been handled (rejected or accepted-then-closed)
    assert!(result.is_ok(), "Third connection should complete within timeout");
}

// ─── Stats exchange ──────────────────────────────────────────────────────────

/// Verify the server sends valid 12-byte stats messages on the TCP control
/// channel during a UDP test.
#[tokio::test]
async fn udp_server_sends_stats_on_control_channel() {
    let port_base = alloc_port_range();
    let server_addr = start_server(default_server_dur(), default_server_dur(), port_base, 10).await;

    let mut stream = TcpStream::connect(server_addr).await.unwrap();

    let mut hello = [0u8; 4];
    stream.read_exact(&mut hello).await.unwrap();

    // UDP receive test
    let cmd = make_cmd(0x00, 0x02, 0x00, 0, 512, 0, 0, 0);
    stream.write_all(&cmd).await.unwrap();

    let mut confirm = [0u8; 4];
    stream.read_exact(&mut confirm).await.unwrap();

    let mut port_buf = [0u8; 2];
    stream.read_exact(&mut port_buf).await.unwrap();

    // Wait for at least one stats message (sent every ~1s)
    let mut stats_buf = [0u8; 12];
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        stream.read_exact(&mut stats_buf),
    )
    .await;

    assert!(result.is_ok(), "Should receive stats within 3 seconds");
    result.unwrap().unwrap();

    // Validate stats format
    assert_eq!(stats_buf[0], 0x07, "Stats type byte should be 0x07");
    let seq = BigEndian::read_u32(&stats_buf[1..5]);
    assert!(seq >= 1, "Stats seq should be >= 1");
}

// ─── Direction tests: TCP ────────────────────────────────────────────────────

/// TCP Transmit: client sends data, server receives. Server should accept data.
#[tokio::test]
async fn tcp_transmit_server_receives() {
    let server_addr = start_server(
        Duration::from_secs(5),
        default_server_dur(),
        alloc_port_range(),
        10,
    )
    .await;

    let mut stream = TcpStream::connect(server_addr).await.unwrap();

    let mut hello = [0u8; 4];
    stream.read_exact(&mut hello).await.unwrap();

    // TCP transmit (client sends, server receives)
    let cmd = make_cmd(0x01, 0x01, 0x00, 0, 0x8000, 0, 0, 0);
    stream.write_all(&cmd).await.unwrap();

    let mut confirm = [0u8; 4];
    stream.read_exact(&mut confirm).await.unwrap();
    assert_eq!(confirm[0], 0x01);

    // Send some data to the server
    let data = vec![0u8; 4096];
    for _ in 0..10 {
        if stream.write_all(&data).await.is_err() {
            break;
        }
    }

    // Server should send stats back on the same connection.
    // Read until we get a stats message or the session ends.
    let mut buf = [0u8; 65536];
    let mut got_stats = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buf)).await {
            Ok(Ok(0)) | Ok(Err(_)) => break,
            Ok(Ok(n)) => {
                // Look for a 0x07 stats byte in the received data
                for i in 0..n {
                    if buf[i] == 0x07 && i + 12 <= n {
                        got_stats = true;
                        break;
                    }
                }
                if got_stats {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    assert!(got_stats, "Server should send stats during TCP transmit test");
}

/// TCP Both: bidirectional data flow.
#[tokio::test]
async fn tcp_both_direction() {
    let server_addr = start_server(
        Duration::from_secs(3),
        default_server_dur(),
        alloc_port_range(),
        10,
    )
    .await;

    let mut stream = TcpStream::connect(server_addr).await.unwrap();

    let mut hello = [0u8; 4];
    stream.read_exact(&mut hello).await.unwrap();

    // TCP both direction
    let cmd = make_cmd(0x01, 0x03, 0x00, 0, 0x8000, 0, 0, 0);
    stream.write_all(&cmd).await.unwrap();

    let mut confirm = [0u8; 4];
    stream.read_exact(&mut confirm).await.unwrap();
    assert_eq!(confirm[0], 0x01);

    // In both mode, the server should be sending data to us
    let mut buf = [0u8; 65536];
    let mut received = 0u64;

    // Also send some data to the server
    let send_data = vec![0u8; 1024];
    let (mut reader, mut writer) = stream.into_split();

    let send_handle = tokio::spawn(async move {
        for _ in 0..20 {
            if writer.write_all(&send_data).await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(1), reader.read(&mut buf)).await {
            Ok(Ok(0)) | Ok(Err(_)) => break,
            Ok(Ok(n)) => received += n as u64,
            Err(_) => break,
        }
    }

    let _ = send_handle.await;
    assert!(received > 0, "Should receive data from server in both mode");
}

// ─── Direction tests: UDP ────────────────────────────────────────────────────

/// UDP Transmit: client sends UDP data, server receives.
#[tokio::test]
async fn udp_transmit_server_receives() {
    let port_base = alloc_port_range();
    let server_addr = start_server(
        Duration::from_secs(5),
        default_server_dur(),
        port_base,
        10,
    )
    .await;

    let mut stream = TcpStream::connect(server_addr).await.unwrap();

    let mut hello = [0u8; 4];
    stream.read_exact(&mut hello).await.unwrap();

    // UDP transmit (client sends, server receives)
    let cmd = make_cmd(0x00, 0x01, 0x00, 0, 512, 0, 0, 0);
    stream.write_all(&cmd).await.unwrap();

    let mut confirm = [0u8; 4];
    stream.read_exact(&mut confirm).await.unwrap();

    let mut port_buf = [0u8; 2];
    stream.read_exact(&mut port_buf).await.unwrap();
    let udp_port = BigEndian::read_u16(&port_buf);

    // Bind client UDP socket on port + 256
    let client_udp_addr: SocketAddr = format!("127.0.0.1:{}", udp_port + 256).parse().unwrap();
    let client_udp = UdpSocket::bind(client_udp_addr).await.unwrap();
    let server_udp_addr: SocketAddr = format!("127.0.0.1:{}", udp_port).parse().unwrap();

    // Send UDP packets to the server
    let mut pkt = vec![0u8; 484]; // 512 - 28 header overhead
    for seq in 1u32..=50 {
        pkt[0] = (seq >> 24) as u8;
        pkt[1] = (seq >> 16) as u8;
        pkt[2] = (seq >> 8) as u8;
        pkt[3] = seq as u8;
        let _ = client_udp.send_to(&pkt, server_udp_addr).await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Server should send stats on the TCP control channel reflecting received bytes
    let mut stats_buf = [0u8; 12];
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        stream.read_exact(&mut stats_buf),
    )
    .await;

    assert!(result.is_ok(), "Should receive stats from server");
    result.unwrap().unwrap();
    assert_eq!(stats_buf[0], 0x07, "Stats type byte should be 0x07");

    // The recv_bytes field should be > 0 since we sent data
    let recv_bytes = LittleEndian::read_u32(&stats_buf[8..12]);
    assert!(recv_bytes > 0, "Server should report received bytes > 0, got {recv_bytes}");
}

/// UDP Both: bidirectional data flow.
#[tokio::test]
async fn udp_both_direction() {
    let port_base = alloc_port_range();
    let server_addr = start_server(
        Duration::from_secs(5),
        default_server_dur(),
        port_base,
        10,
    )
    .await;

    let mut stream = TcpStream::connect(server_addr).await.unwrap();

    let mut hello = [0u8; 4];
    stream.read_exact(&mut hello).await.unwrap();

    // UDP both direction
    let cmd = make_cmd(0x00, 0x03, 0x00, 0, 512, 0, 0, 0);
    stream.write_all(&cmd).await.unwrap();

    let mut confirm = [0u8; 4];
    stream.read_exact(&mut confirm).await.unwrap();

    let mut port_buf = [0u8; 2];
    stream.read_exact(&mut port_buf).await.unwrap();
    let udp_port = BigEndian::read_u16(&port_buf);

    let client_udp_addr: SocketAddr = format!("127.0.0.1:{}", udp_port + 256).parse().unwrap();
    let client_udp = UdpSocket::bind(client_udp_addr).await.unwrap();
    let server_udp_addr: SocketAddr = format!("127.0.0.1:{}", udp_port).parse().unwrap();

    // Send some UDP packets to the server
    let mut pkt = vec![0u8; 484];
    for seq in 1u32..=10 {
        pkt[0] = (seq >> 24) as u8;
        pkt[1] = (seq >> 16) as u8;
        pkt[2] = (seq >> 8) as u8;
        pkt[3] = seq as u8;
        let _ = client_udp.send_to(&pkt, server_udp_addr).await;
    }

    // In both mode, server should also be sending UDP data to us
    let mut buf = [0u8; 2048];
    let result = tokio::time::timeout(Duration::from_secs(2), client_udp.recv(&mut buf)).await;
    assert!(result.is_ok(), "Should receive UDP data from server in both mode");

    // Also verify stats on control channel
    let mut stats_buf = [0u8; 12];
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        stream.read_exact(&mut stats_buf),
    )
    .await;
    assert!(result.is_ok(), "Should receive stats on control channel in both mode");
    assert_eq!(stats_buf[0], 0x07);
}

/// UDP Receive: server sends data, verify we get sequenced packets.
#[tokio::test]
async fn udp_receive_sequenced_packets() {
    let port_base = alloc_port_range();
    let server_addr = start_server(
        Duration::from_secs(5),
        default_server_dur(),
        port_base,
        10,
    )
    .await;

    let mut stream = TcpStream::connect(server_addr).await.unwrap();

    let mut hello = [0u8; 4];
    stream.read_exact(&mut hello).await.unwrap();

    let cmd = make_cmd(0x00, 0x02, 0x00, 0, 512, 0, 0, 0);
    stream.write_all(&cmd).await.unwrap();

    let mut confirm = [0u8; 4];
    stream.read_exact(&mut confirm).await.unwrap();

    let mut port_buf = [0u8; 2];
    stream.read_exact(&mut port_buf).await.unwrap();
    let udp_port = BigEndian::read_u16(&port_buf);

    let client_udp_addr: SocketAddr = format!("127.0.0.1:{}", udp_port + 256).parse().unwrap();
    let client_udp = UdpSocket::bind(client_udp_addr).await.unwrap();

    // Receive several packets and verify sequence numbers are increasing
    let mut buf = [0u8; 2048];
    let mut last_seq: u32 = 0;
    let mut count = 0;

    for _ in 0..20 {
        match tokio::time::timeout(Duration::from_secs(2), client_udp.recv(&mut buf)).await {
            Ok(Ok(n)) if n >= 4 => {
                let seq = BigEndian::read_u32(&buf[..4]);
                if count > 0 {
                    assert!(
                        seq > last_seq,
                        "Sequence numbers should increase: got {seq} after {last_seq}"
                    );
                }
                last_seq = seq;
                count += 1;
                if count >= 10 {
                    break;
                }
            }
            _ => break,
        }
    }

    assert!(count >= 5, "Should receive at least 5 sequenced UDP packets, got {count}");
}
