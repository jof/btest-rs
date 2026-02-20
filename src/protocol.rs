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
