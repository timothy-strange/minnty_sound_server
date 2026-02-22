use bytes::{BufMut, Bytes, BytesMut};

pub const MAGIC: [u8; 4] = *b"MNTY";
pub const VERSION: u8 = 1;

pub const MSG_HELLO: u8 = 1;
pub const MSG_CONFIG: u8 = 2;
pub const MSG_KEEPALIVE: u8 = 3;

#[derive(Clone, Copy, Debug)]
pub struct StreamConfig {
    pub udp_port: u16,
    pub sample_rate: u32,
    pub channels: u8,
    pub frame_size: usize,
    pub pcm_queue_depth: usize,
}

pub const DEFAULT_STREAM_CONFIG: StreamConfig = StreamConfig {
    udp_port: 40110,
    sample_rate: 48_000,
    channels: 2,
    frame_size: 960,
    pcm_queue_depth: 64,
};

#[derive(Clone, Copy, Debug)]
pub enum ControlMessage {
    Hello,
    KeepAlive,
}

pub fn parse_control_packet(data: &[u8]) -> Option<ControlMessage> {
    if data.len() < 6 {
        return None;
    }

    if data[0..4] != MAGIC {
        return None;
    }

    if data[4] != VERSION {
        return None;
    }

    match data[5] {
        MSG_HELLO => Some(ControlMessage::Hello),
        MSG_KEEPALIVE => Some(ControlMessage::KeepAlive),
        _ => None,
    }
}

pub fn build_config_packet(config: StreamConfig) -> Bytes {
    debug_assert!(config.frame_size <= u16::MAX as usize);
    let mut buf = BytesMut::with_capacity(4 + 1 + 1 + 2 + 4 + 1 + 2);
    buf.put_slice(&MAGIC);
    buf.put_u8(VERSION);
    buf.put_u8(MSG_CONFIG);
    buf.put_u16(config.udp_port);
    buf.put_u32(config.sample_rate);
    buf.put_u8(config.channels);
    buf.put_u16(config.frame_size as u16);
    buf.freeze()
}

pub fn build_stream_packet(seq: u32, timestamp_ms: u64, payload: &[u8]) -> Bytes {
    // Stream packets intentionally omit MAGIC because only payload stream packets are
    // sent on this path, and the receiver parser for stream frames expects this header.
    let mut buf = BytesMut::with_capacity(1 + 4 + 8 + 2 + payload.len());
    buf.put_u8(VERSION);
    buf.put_u32(seq);
    buf.put_u64(timestamp_ms);
    buf.put_u16(payload.len() as u16);
    buf.put_slice(payload);
    buf.freeze()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_control_packets() {
        let mut hello = Vec::new();
        hello.extend_from_slice(&MAGIC);
        hello.push(VERSION);
        hello.push(MSG_HELLO);
        assert!(matches!(
            parse_control_packet(&hello),
            Some(ControlMessage::Hello)
        ));

        let mut keepalive = Vec::new();
        keepalive.extend_from_slice(&MAGIC);
        keepalive.push(VERSION);
        keepalive.push(MSG_KEEPALIVE);
        assert!(matches!(
            parse_control_packet(&keepalive),
            Some(ControlMessage::KeepAlive)
        ));

        let mut invalid = Vec::new();
        invalid.extend_from_slice(b"NOPE");
        invalid.push(VERSION);
        invalid.push(MSG_HELLO);
        assert!(parse_control_packet(&invalid).is_none());
    }

    #[test]
    fn build_config_packet_contains_fields() {
        let packet = build_config_packet(DEFAULT_STREAM_CONFIG);
        let bytes = packet.as_ref();
        assert_eq!(&bytes[0..4], MAGIC.as_slice());
        assert_eq!(bytes[4], VERSION);
        assert_eq!(bytes[5], MSG_CONFIG);
    }

    #[test]
    fn build_stream_packet_has_payload_length() {
        let payload = [1u8, 2, 3, 4, 5];
        let packet = build_stream_packet(42, 1000, &payload);
        let bytes = packet.as_ref();
        assert_eq!(bytes[0], VERSION);
        let len = u16::from_be_bytes([bytes[13], bytes[14]]);
        assert_eq!(len as usize, payload.len());
        let seq = u32::from_be_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]);
        assert_eq!(seq, 42);
        let ts = u64::from_be_bytes([
            bytes[5], bytes[6], bytes[7], bytes[8], bytes[9], bytes[10], bytes[11], bytes[12],
        ]);
        assert_eq!(ts, 1000);
    }
}
