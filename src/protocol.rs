use byteorder::{BigEndian, ByteOrder, LittleEndian};
use std::fmt;

pub const BTEST_PORT: u16 = 2000;
pub const BTEST_PORT_CLIENT_OFFSET: u16 = 256;

// Message types (first byte of 4-byte messages)
pub const MSG_HELLO: [u8; 4] = [0x01, 0x00, 0x00, 0x00];
pub const MSG_AUTH_OK: [u8; 4] = [0x01, 0x00, 0x00, 0x00];
#[allow(dead_code)] // Reserved for future auth implementation
pub const MSG_AUTH_FAIL: [u8; 4] = [0x00, 0x00, 0x00, 0x00];
#[allow(dead_code)] // Reserved for future auth implementation
pub const MSG_AUTH_REQUIRED_OLD: [u8; 4] = [0x02, 0x00, 0x00, 0x00];
#[allow(dead_code)] // Reserved for future auth implementation
pub const MSG_AUTH_REQUIRED_NEW: [u8; 4] = [0x03, 0x00, 0x00, 0x00];
pub const MSG_STAT: u8 = 0x07;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Udp,
    Tcp,
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Protocol::Udp => write!(f, "UDP"),
            Protocol::Tcp => write!(f, "TCP"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Transmit, // Client transmits to server (server receives)
    Receive,  // Client receives from server (server transmits)
    Both,
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Direction::Transmit => write!(f, "transmit"),
            Direction::Receive => write!(f, "receive"),
            Direction::Both => write!(f, "both"),
        }
    }
}

/// The 16-byte command structure sent by the client after the hello handshake.
///
/// Wire format (all multi-byte integers are little-endian unless noted):
/// ```text
/// byte[0]:    protocol        0x01=TCP, 0x00=UDP
/// byte[1]:    direction       0x01=transmit, 0x02=receive, 0x03=both
/// byte[2]:    random_data     0x00=random, 0x01=zeros
/// byte[3]:    tcp_conn_count  0 means 1 connection, else N connections
/// byte[4:5]:  tx_size         uint16 LE (UDP packet size, or 0x8000 for TCP)
/// byte[6:7]:  client_buf_size uint16 LE
/// byte[8:11]: remote_tx_speed uint32 LE (0 = unlimited)
/// byte[12:15]:local_tx_speed  uint32 LE (0 = unlimited)
/// ```
#[derive(Debug, Clone)]
pub struct Command {
    pub protocol: Protocol,
    pub direction: Direction,
    pub random_data: bool,
    pub tcp_conn_count: u8,
    pub tx_size: u16,
    #[allow(dead_code)] // Protocol field, not yet used
    pub client_buf_size: u16,
    pub remote_tx_speed: u32,
    pub local_tx_speed: u32,
}

impl Command {
    pub fn parse(buf: &[u8; 16]) -> Option<Command> {
        let protocol = match buf[0] {
            0x00 => Protocol::Udp,
            0x01 => Protocol::Tcp,
            _ => return None,
        };

        let direction = match buf[1] {
            0x01 => Direction::Transmit,
            0x02 => Direction::Receive,
            0x03 => Direction::Both,
            _ => return None,
        };

        Some(Command {
            protocol,
            direction,
            random_data: buf[2] == 0x00, // 0x00 = random, 0x01 = zeros
            tcp_conn_count: buf[3],
            tx_size: LittleEndian::read_u16(&buf[4..6]),
            client_buf_size: LittleEndian::read_u16(&buf[6..8]),
            remote_tx_speed: LittleEndian::read_u32(&buf[8..12]),
            local_tx_speed: LittleEndian::read_u32(&buf[12..16]),
        })
    }

    pub fn effective_conn_count(&self) -> u32 {
        if self.tcp_conn_count == 0 {
            1
        } else {
            self.tcp_conn_count as u32
        }
    }
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "proto={} dir={} random={} conns={} tx_size={} remote_speed={} local_speed={}",
            self.protocol,
            self.direction,
            self.random_data,
            self.effective_conn_count(),
            self.tx_size,
            if self.remote_tx_speed == 0 {
                "unlimited".to_string()
            } else {
                format!("{}", self.remote_tx_speed)
            },
            if self.local_tx_speed == 0 {
                "unlimited".to_string()
            } else {
                format!("{}", self.local_tx_speed)
            },
        )
    }
}

/// Statistics message sent every ~1 second on the TCP control channel.
///
/// Wire format (12 bytes):
/// ```text
/// byte[0]:    0x07 (message type)
/// byte[1:4]:  sequence number (uint32 big-endian)
/// byte[5:7]:  padding/unknown (zeros)
/// byte[8:11]: bytes received this interval (uint32 little-endian)
/// ```
#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub seq: u32,
    pub recv_bytes: u32,
}

impl Stats {
    pub fn parse(buf: &[u8; 12]) -> Option<Stats> {
        if buf[0] != MSG_STAT {
            return None;
        }
        Some(Stats {
            seq: BigEndian::read_u32(&buf[1..5]),
            recv_bytes: LittleEndian::read_u32(&buf[8..12]),
        })
    }

    pub fn serialize(&self) -> [u8; 12] {
        let mut buf = [0u8; 12];
        buf[0] = MSG_STAT;
        BigEndian::write_u32(&mut buf[1..5], self.seq);
        // bytes 5-7 are padding (zeros)
        LittleEndian::write_u32(&mut buf[8..12], self.recv_bytes);
        buf
    }
}

/// Multi-connection confirm message.
///
/// When `tcp_conn_count` > 1, the server responds with a 4-byte confirm
/// that includes a session token the secondary connections use to identify
/// themselves.
#[derive(Debug, Clone)]
pub struct ConfirmMulti {
    pub token: [u8; 4],
}

impl ConfirmMulti {
    /// Build confirm for multi-connection mode.
    /// Format: 01:xx:yy:00 where xx:yy is a session identifier.
    pub fn new(session_id: u16) -> Self {
        let mut token = [0x01, 0x00, 0x00, 0x00];
        token[1] = (session_id >> 8) as u8;
        token[2] = (session_id & 0xFF) as u8;
        Self { token }
    }

    /// Build confirm for single-connection mode.
    pub fn single() -> Self {
        Self { token: MSG_AUTH_OK }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Command::parse ---

    fn make_cmd_buf(
        proto: u8,
        dir: u8,
        random: u8,
        conns: u8,
        tx_size: u16,
        buf_size: u16,
        remote_speed: u32,
        local_speed: u32,
    ) -> [u8; 16] {
        let mut buf = [0u8; 16];
        buf[0] = proto;
        buf[1] = dir;
        buf[2] = random;
        buf[3] = conns;
        LittleEndian::write_u16(&mut buf[4..6], tx_size);
        LittleEndian::write_u16(&mut buf[6..8], buf_size);
        LittleEndian::write_u32(&mut buf[8..12], remote_speed);
        LittleEndian::write_u32(&mut buf[12..16], local_speed);
        buf
    }

    #[test]
    fn parse_udp_receive_command() {
        let buf = make_cmd_buf(0x00, 0x02, 0x00, 0, 1500, 512, 0, 0);
        let cmd = Command::parse(&buf).unwrap();
        assert_eq!(cmd.protocol, Protocol::Udp);
        assert_eq!(cmd.direction, Direction::Receive);
        assert!(cmd.random_data);
        assert_eq!(cmd.tcp_conn_count, 0);
        assert_eq!(cmd.tx_size, 1500);
        assert_eq!(cmd.client_buf_size, 512);
        assert_eq!(cmd.remote_tx_speed, 0);
        assert_eq!(cmd.local_tx_speed, 0);
    }

    #[test]
    fn parse_tcp_transmit_command() {
        let buf = make_cmd_buf(0x01, 0x01, 0x01, 20, 0x8000, 256, 1_000_000, 500_000);
        let cmd = Command::parse(&buf).unwrap();
        assert_eq!(cmd.protocol, Protocol::Tcp);
        assert_eq!(cmd.direction, Direction::Transmit);
        assert!(!cmd.random_data); // 0x01 = zeros
        assert_eq!(cmd.tcp_conn_count, 20);
        assert_eq!(cmd.tx_size, 0x8000);
        assert_eq!(cmd.remote_tx_speed, 1_000_000);
        assert_eq!(cmd.local_tx_speed, 500_000);
    }

    #[test]
    fn parse_both_direction() {
        let buf = make_cmd_buf(0x00, 0x03, 0x00, 0, 1400, 0, 0, 0);
        let cmd = Command::parse(&buf).unwrap();
        assert_eq!(cmd.direction, Direction::Both);
    }

    #[test]
    fn parse_invalid_protocol() {
        let buf = make_cmd_buf(0x05, 0x01, 0x00, 0, 0, 0, 0, 0);
        assert!(Command::parse(&buf).is_none());
    }

    #[test]
    fn parse_invalid_direction() {
        let buf = make_cmd_buf(0x00, 0x00, 0x00, 0, 0, 0, 0, 0);
        assert!(Command::parse(&buf).is_none());

        let buf = make_cmd_buf(0x00, 0x04, 0x00, 0, 0, 0, 0, 0);
        assert!(Command::parse(&buf).is_none());
    }

    // --- Command::effective_conn_count ---

    #[test]
    fn effective_conn_count_zero_means_one() {
        let buf = make_cmd_buf(0x01, 0x01, 0x00, 0, 0x8000, 0, 0, 0);
        let cmd = Command::parse(&buf).unwrap();
        assert_eq!(cmd.effective_conn_count(), 1);
    }

    #[test]
    fn effective_conn_count_nonzero() {
        let buf = make_cmd_buf(0x01, 0x01, 0x00, 20, 0x8000, 0, 0, 0);
        let cmd = Command::parse(&buf).unwrap();
        assert_eq!(cmd.effective_conn_count(), 20);
    }

    // --- Command Display ---

    #[test]
    fn command_display_unlimited() {
        let buf = make_cmd_buf(0x00, 0x02, 0x00, 0, 1500, 0, 0, 0);
        let cmd = Command::parse(&buf).unwrap();
        let s = format!("{}", cmd);
        assert!(s.contains("proto=UDP"));
        assert!(s.contains("dir=receive"));
        assert!(s.contains("remote_speed=unlimited"));
        assert!(s.contains("local_speed=unlimited"));
    }

    #[test]
    fn command_display_with_speeds() {
        let buf = make_cmd_buf(0x01, 0x01, 0x01, 5, 0x8000, 0, 100_000, 200_000);
        let cmd = Command::parse(&buf).unwrap();
        let s = format!("{}", cmd);
        assert!(s.contains("proto=TCP"));
        assert!(s.contains("dir=transmit"));
        assert!(s.contains("conns=5"));
        assert!(s.contains("remote_speed=100000"));
        assert!(s.contains("local_speed=200000"));
    }

    // --- Protocol / Direction Display ---

    #[test]
    fn protocol_display() {
        assert_eq!(format!("{}", Protocol::Udp), "UDP");
        assert_eq!(format!("{}", Protocol::Tcp), "TCP");
    }

    #[test]
    fn direction_display() {
        assert_eq!(format!("{}", Direction::Transmit), "transmit");
        assert_eq!(format!("{}", Direction::Receive), "receive");
        assert_eq!(format!("{}", Direction::Both), "both");
    }

    // --- Stats parse/serialize roundtrip ---

    #[test]
    fn stats_roundtrip() {
        let original = Stats {
            seq: 42,
            recv_bytes: 1_048_576,
        };
        let wire = original.serialize();
        let parsed = Stats::parse(&wire).unwrap();
        assert_eq!(parsed.seq, 42);
        assert_eq!(parsed.recv_bytes, 1_048_576);
    }

    #[test]
    fn stats_serialize_format() {
        let stat = Stats {
            seq: 1,
            recv_bytes: 256,
        };
        let buf = stat.serialize();
        assert_eq!(buf[0], MSG_STAT);
        // seq=1 big-endian in bytes 1..5
        assert_eq!(&buf[1..5], &[0, 0, 0, 1]);
        // padding zeros
        assert_eq!(&buf[5..8], &[0, 0, 0]);
        // recv_bytes=256 little-endian in bytes 8..12
        assert_eq!(&buf[8..12], &[0, 1, 0, 0]);
    }

    #[test]
    fn stats_parse_wrong_type_byte() {
        let mut buf = [0u8; 12];
        buf[0] = 0x01; // not MSG_STAT
        assert!(Stats::parse(&buf).is_none());
    }

    #[test]
    fn stats_roundtrip_large_values() {
        let original = Stats {
            seq: u32::MAX,
            recv_bytes: u32::MAX,
        };
        let wire = original.serialize();
        let parsed = Stats::parse(&wire).unwrap();
        assert_eq!(parsed.seq, u32::MAX);
        assert_eq!(parsed.recv_bytes, u32::MAX);
    }

    #[test]
    fn stats_roundtrip_zero() {
        let original = Stats {
            seq: 0,
            recv_bytes: 0,
        };
        let wire = original.serialize();
        let parsed = Stats::parse(&wire).unwrap();
        assert_eq!(parsed.seq, 0);
        assert_eq!(parsed.recv_bytes, 0);
    }

    // --- ConfirmMulti ---

    #[test]
    fn confirm_single() {
        let c = ConfirmMulti::single();
        assert_eq!(c.token, MSG_AUTH_OK);
    }

    #[test]
    fn confirm_multi_session_id() {
        let c = ConfirmMulti::new(0x0102);
        assert_eq!(c.token[0], 0x01);
        assert_eq!(c.token[1], 0x01); // high byte of 0x0102
        assert_eq!(c.token[2], 0x02); // low byte of 0x0102
        assert_eq!(c.token[3], 0x00);
    }

    #[test]
    fn confirm_multi_session_zero() {
        let c = ConfirmMulti::new(0);
        assert_eq!(c.token, [0x01, 0x00, 0x00, 0x00]);
    }

    // --- Constants ---

    #[test]
    fn constants_sanity() {
        assert_eq!(BTEST_PORT, 2000);
        assert_eq!(BTEST_PORT_CLIENT_OFFSET, 256);
        assert_eq!(MSG_HELLO, MSG_AUTH_OK);
        assert_ne!(MSG_AUTH_OK, MSG_AUTH_FAIL);
    }
}
