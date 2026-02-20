# btest-rs

High-performance MikroTik bandwidth-test (btest) protocol server written in Rust.

Implements the MikroTik `/tool bandwidth-test` server protocol, allowing MikroTik
devices to run bandwidth tests against a Linux host.

## Features

- **TCP mode**: single and multi-connection (connection-count 1–20+)
- **UDP mode**: with correct port negotiation (port + 256 client offset)
- **All directions**: transmit, receive, both
- **Async I/O**: built on tokio for high concurrency
- **Zero-copy data path**: minimal overhead for maximum throughput

## Quick Start

```bash
cargo build --release
./target/release/btest-rs --listen 0.0.0.0:2000
```

Then from a MikroTik device:
```
/tool bandwidth-test <server-ip> protocol=tcp direction=receive duration=10s
/tool bandwidth-test <server-ip> protocol=udp direction=receive duration=10s remote-udp-tx-size=1500
```

## CLI Options

```
btest-rs [OPTIONS]

Options:
  -l, --listen <ADDR>          Listen address [default: 0.0.0.0:2000]
      --udp-port-start <PORT>  Starting UDP port for allocation [default: 2000]
      --max-sessions <N>       Maximum concurrent sessions [default: 100]
```

Set `RUST_LOG=debug` for verbose protocol logging.

## Protocol

Implements the MikroTik bandwidth-test protocol on TCP port 2000.
Protocol was reverse-engineered via packet captures against RouterOS 7.20.8
and the [btest-opensource](https://github.com/samm-git/btest-opensource) project.

### Handshake (TCP port 2000)

1. **Server → Client**: `01:00:00:00` (hello)
2. **Client → Server**: 16-byte command struct
3. **Server → Client**: `01:xx:yy:00` (confirm; xx:yy = session token for multi-conn)

### 16-byte Command Structure (little-endian)

| Offset | Size | Field | Description |
|--------|------|-------|-------------|
| 0      | 1    | protocol | `0x01`=TCP, `0x00`=UDP |
| 1      | 1    | direction | `0x01`=transmit, `0x02`=receive, `0x03`=both |
| 2      | 1    | random_data | `0x00`=random, `0x01`=zeros |
| 3      | 1    | tcp_conn_count | 0=single, N=N connections |
| 4-5    | 2    | tx_size | uint16 LE (UDP pkt size, or `0x8000` for TCP) |
| 6-7    | 2    | client_buf_size | uint16 LE |
| 8-11   | 4    | remote_tx_speed | uint32 LE (0=unlimited) |
| 12-15  | 4    | local_tx_speed | uint32 LE (0=unlimited) |

### UDP Mode

- After handshake, server sends 2-byte port number (big-endian) on TCP control
- Server binds UDP on that port; client listens on `port + 256`
- Data packets: 4-byte big-endian sequence number + payload

### Stats Message (12 bytes, exchanged every ~1s on TCP control)

| Offset | Size | Field | Description |
|--------|------|-------|-------------|
| 0      | 1    | type | `0x07` |
| 1-4    | 4    | seq | uint32 big-endian |
| 5-7    | 3    | padding | zeros |
| 8-11   | 4    | recv_bytes | uint32 little-endian |

## Not Yet Implemented

- Authentication (old MD5 challenge-response or new EC-SRP5)
- Rate limiting (`remote-tx-speed` / `local-tx-speed`)
- Systemd service unit / packaging

## License

BSD 2-Clause. See [LICENSE](LICENSE).
