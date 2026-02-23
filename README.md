# 🚀 btest-rs

> *Because your MikroTik deserves a speed test server that goes brrrrr*

High-performance MikroTik bandwidth-test (btest) protocol server, lovingly written in Rust. 🦀

Implements the MikroTik `/tool bandwidth-test` server protocol, so your RouterOS
devices can fling packets at a Linux host and feel good about it.

## ✨ Features

- 🔌 **TCP mode** — single and multi-connection (connection-count 1–20+)
- 📡 **UDP mode** — with correct port negotiation (port + 256 client offset)
- ↔️ **All directions** — transmit, receive, both — we don't judge
- ⚡ **Async I/O** — built on tokio for high concurrency
- 🏎️ **Zero-copy data path** — minimal overhead, maximum go-fast

## 🏁 Quick Start

```bash
cargo build --release
./target/release/btest-rs --listen 0.0.0.0:2000
```

## 📦 Installation

Grab a prebuilt binary from [GitHub Releases](https://github.com/jof/btest-rs/releases)
and you're off to the races:

```bash
curl -sL https://github.com/jof/btest-rs/releases/latest/download/btest-rs-v0.1.0-x86_64-unknown-linux-gnu.tar.gz \
  | sudo tar xz -C /usr/local/bin --strip-components=1 btest-rs-v0.1.0-x86_64-unknown-linux-gnu/btest-rs
sudo cp btest-rs.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now btest-rs
```

A sample `btest-rs.service` systemd unit is included in the repo. Tweak it to taste. 🧑‍🍳

Then from a MikroTik device, let 'er rip:
```
/tool bandwidth-test <server-ip> protocol=tcp direction=receive duration=10s
/tool bandwidth-test <server-ip> protocol=udp direction=receive duration=10s remote-udp-tx-size=1500
```

## 🎛️ CLI Options

```
btest-rs [OPTIONS]

Options:
  -l, --listen <ADDR>              Listen address [default: 0.0.0.0:2000]
      --udp-port-start <PORT>      Starting UDP port for allocation [default: 2000]
      --max-sessions <N>           Maximum concurrent sessions [default: 100]
      --max-test-duration <SECS>   Hard cap on test duration per session [default: 300]
      --control-timeout <SECS>     Idle timeout on TCP control channel [default: 30]
```

Set `RUST_LOG=debug` for verbose protocol logging. 🔍

## 🔬 Protocol

Implements the MikroTik bandwidth-test protocol on TCP port 2000.
Protocol was reverse-engineered via packet captures against RouterOS 7.20.8
and the [btest-opensource](https://github.com/samm-git/btest-opensource) project.
(Thanks, Wireshark. You're the real MVP. 🦈)

### 🤝 Handshake (TCP port 2000)

1. **Server → Client**: `01:00:00:00` (hello! 👋)
2. **Client → Server**: 16-byte command struct (what do you want?)
3. **Server → Client**: `01:xx:yy:00` (confirm; xx:yy = session token for multi-conn)

### 📦 16-byte Command Structure (little-endian)

| Offset | Size | Field | Description |
|--------|------|-------|-------------|
| 0      | 1    | protocol | `0x01`=TCP, `0x00`=UDP |
| 1      | 1    | direction | `0x01`=transmit, `0x02`=receive, `0x03`=both |
| 2      | 1    | random_data | `0x00`=random, `0x01`=zeros |
| 3      | 1    | tcp_conn_count | 0=single, N=N connections |
| 4-5    | 2    | tx_size | uint16 LE (UDP pkt size, or `0x8000` for TCP) |
| 6-7    | 2    | client_buf_size | uint16 LE |
| 8-11   | 4    | remote_tx_speed | uint32 LE (0=unlimited 🏎️) |
| 12-15  | 4    | local_tx_speed | uint32 LE (0=unlimited 🏎️) |

### 📡 UDP Mode

- After handshake, server sends 2-byte port number (big-endian) on TCP control
- Server binds UDP on that port; client listens on `port + 256`
- Data packets: 4-byte big-endian sequence number + payload

### 📊 Stats Message (12 bytes, exchanged every ~1s on TCP control)

| Offset | Size | Field | Description |
|--------|------|-------|-------------|
| 0      | 1    | type | `0x07` |
| 1-4    | 4    | seq | uint32 big-endian |
| 5-7    | 3    | padding | zeros |
| 8-11   | 4    | recv_bytes | uint32 little-endian |

## 🚧 Not Yet Implemented

- 🔐 Authentication (old MD5 challenge-response or new EC-SRP5)
- 🐌 Rate limiting (`remote-tx-speed` / `local-tx-speed`)
- 📦 ~~Systemd service unit / packaging~~ — shipped! 🎁

PRs welcome! Or just open an issue and yell about it. 🎉

## 📄 License

BSD 2-Clause. See [LICENSE](LICENSE).
