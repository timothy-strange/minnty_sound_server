use bytes::{BufMut, Bytes, BytesMut};

pub const MAGIC: [u8; 4] = *b"MNTY";
pub const VERSION: u8 = 1;

pub const MSG_HELLO: u8 = 1;
pub const MSG_CONFIG: u8 = 2;
pub const MSG_KEEPALIVE: u8 = 3;
pub const MSG_STATUS: u8 = 4;
pub const MSG_STATUS_REQUEST: u8 = 5;
pub const MSG_TIME_SYNC_REQUEST: u8 = 6;
pub const MSG_TIME_SYNC_RESPONSE: u8 = 7;
pub const MSG_MEDIA_CONTROL: u8 = 8;
pub const MSG_NOW_PLAYING: u8 = 9;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MediaCommand {
    PlayPause,
    Play,
    Pause,
    Next,
    Previous,
    SeekRelativeMs,
    VolumeUp,
    VolumeDown,
    SeekAbsoluteMs,
    Stop,
}

impl MediaCommand {
    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::PlayPause),
            2 => Some(Self::Play),
            3 => Some(Self::Pause),
            4 => Some(Self::Next),
            5 => Some(Self::Previous),
            6 => Some(Self::SeekRelativeMs),
            7 => Some(Self::VolumeUp),
            8 => Some(Self::VolumeDown),
            9 => Some(Self::SeekAbsoluteMs),
            10 => Some(Self::Stop),
            _ => None,
        }
    }

    pub fn code(self) -> u8 {
        match self {
            Self::PlayPause => 1,
            Self::Play => 2,
            Self::Pause => 3,
            Self::Next => 4,
            Self::Previous => 5,
            Self::SeekRelativeMs => 6,
            Self::VolumeUp => 7,
            Self::VolumeDown => 8,
            Self::SeekAbsoluteMs => 9,
            Self::Stop => 10,
        }
    }

    pub fn is_volume(self) -> bool {
        matches!(self, Self::VolumeUp | Self::VolumeDown)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct StreamConfig {
    pub udp_port: u16,
    pub sample_rate: u32,
    pub channels: u8,
    pub frame_size: usize,
    pub pcm_queue_depth: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaybackStatus {
    Unknown = 0,
    Playing = 1,
    Paused = 2,
    Stopped = 3,
}

impl PlaybackStatus {
    pub fn code(self) -> u8 {
        self as u8
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NowPlayingMetadata {
    pub artist: String,
    pub title: String,
    pub playback_status: PlaybackStatus,
    pub position_ms: Option<u64>,
    pub duration_ms: Option<u64>,
    pub track_id: Option<String>,
}

pub const DEFAULT_STREAM_CONFIG: StreamConfig = StreamConfig {
    udp_port: 40110,
    sample_rate: 48_000,
    channels: 2,
    frame_size: 480,
    pcm_queue_depth: 64,
};

#[derive(Clone, Copy, Debug)]
pub enum ControlMessage {
    Hello,
    KeepAlive,
    Status,
    StatusRequest,
    TimeSyncRequest { client_send_ms: u64 },
    MediaControl { command: MediaCommand, argument: i64 },
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
        MSG_STATUS => Some(ControlMessage::Status),
        MSG_STATUS_REQUEST => Some(ControlMessage::StatusRequest),
        MSG_TIME_SYNC_REQUEST => {
            if data.len() < 14 {
                return None;
            }
            Some(ControlMessage::TimeSyncRequest {
                client_send_ms: u64::from_be_bytes([
                    data[6], data[7], data[8], data[9], data[10], data[11], data[12], data[13],
                ]),
            })
        }
        MSG_MEDIA_CONTROL => {
            if data.len() < 15 {
                return None;
            }
            Some(ControlMessage::MediaControl {
                command: MediaCommand::from_code(data[6])?,
                argument: i64::from_be_bytes([
                    data[7], data[8], data[9], data[10], data[11], data[12], data[13], data[14],
                ]),
            })
        }
        _ => None,
    }
}

pub fn build_media_control_packet(command: MediaCommand, argument: i64) -> Bytes {
    let mut buf = BytesMut::with_capacity(4 + 1 + 1 + 1 + 8);
    buf.put_slice(&MAGIC);
    buf.put_u8(VERSION);
    buf.put_u8(MSG_MEDIA_CONTROL);
    buf.put_u8(command.code());
    buf.put_i64(argument);
    buf.freeze()
}

pub fn build_now_playing_packet(sequence: u64, metadata: &NowPlayingMetadata) -> Bytes {
    let artist = metadata.artist.as_bytes();
    let title = metadata.title.as_bytes();
    let artist_len = artist.len().min(u16::MAX as usize);
    let title_len = title.len().min(u16::MAX as usize);
    let mut buf = BytesMut::with_capacity(4 + 1 + 1 + 8 + 1 + 2 + 2 + artist_len + title_len + 16);
    buf.put_slice(&MAGIC);
    buf.put_u8(VERSION);
    buf.put_u8(MSG_NOW_PLAYING);
    buf.put_u64(sequence);
    buf.put_u8(metadata.playback_status.code());
    buf.put_u16(artist_len as u16);
    buf.put_u16(title_len as u16);
    buf.put_slice(&artist[..artist_len]);
    buf.put_slice(&title[..title_len]);
    buf.put_u64(metadata.position_ms.unwrap_or(u64::MAX));
    buf.put_u64(metadata.duration_ms.unwrap_or(u64::MAX));
    buf.freeze()
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

pub fn build_status_packet(streaming: bool, session_id: u64) -> Bytes {
    let mut buf = BytesMut::with_capacity(4 + 1 + 1 + 1 + 8);
    buf.put_slice(&MAGIC);
    buf.put_u8(VERSION);
    buf.put_u8(MSG_STATUS);
    buf.put_u8(u8::from(streaming));
    buf.put_u64(session_id);
    buf.freeze()
}

pub fn build_time_sync_response_packet(
    client_send_ms: u64,
    server_receive_ms: u64,
    server_send_ms: u64,
) -> Bytes {
    let mut buf = BytesMut::with_capacity(4 + 1 + 1 + 8 + 8 + 8);
    buf.put_slice(&MAGIC);
    buf.put_u8(VERSION);
    buf.put_u8(MSG_TIME_SYNC_RESPONSE);
    buf.put_u64(client_send_ms);
    buf.put_u64(server_receive_ms);
    buf.put_u64(server_send_ms);
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

        let mut status = Vec::new();
        status.extend_from_slice(&MAGIC);
        status.push(VERSION);
        status.push(MSG_STATUS);
        assert!(matches!(
            parse_control_packet(&status),
            Some(ControlMessage::Status)
        ));

        let mut status_request = Vec::new();
        status_request.extend_from_slice(&MAGIC);
        status_request.push(VERSION);
        status_request.push(MSG_STATUS_REQUEST);
        assert!(matches!(
            parse_control_packet(&status_request),
            Some(ControlMessage::StatusRequest)
        ));

        let mut time_sync_request = Vec::new();
        time_sync_request.extend_from_slice(&MAGIC);
        time_sync_request.push(VERSION);
        time_sync_request.push(MSG_TIME_SYNC_REQUEST);
        time_sync_request.extend_from_slice(&0x0102_0304_0506_0708u64.to_be_bytes());
        assert!(matches!(
            parse_control_packet(&time_sync_request),
            Some(ControlMessage::TimeSyncRequest { client_send_ms }) if client_send_ms == 0x0102_0304_0506_0708
        ));

        let media_control = build_media_control_packet(MediaCommand::SeekRelativeMs, -30_000);
        assert!(matches!(
            parse_control_packet(&media_control),
            Some(ControlMessage::MediaControl { command: MediaCommand::SeekRelativeMs, argument: -30_000 })
        ));

        let absolute_seek = build_media_control_packet(MediaCommand::SeekAbsoluteMs, 60_000);
        assert!(matches!(
            parse_control_packet(&absolute_seek),
            Some(ControlMessage::MediaControl { command: MediaCommand::SeekAbsoluteMs, argument: 60_000 })
        ));

        let stop = build_media_control_packet(MediaCommand::Stop, 0);
        assert!(matches!(
            parse_control_packet(&stop),
            Some(ControlMessage::MediaControl { command: MediaCommand::Stop, argument: 0 })
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

    #[test]
    fn build_status_packet_contains_state_and_session() {
        let packet = build_status_packet(true, 0x1122_3344_5566_7788);
        let bytes = packet.as_ref();
        assert_eq!(&bytes[0..4], MAGIC.as_slice());
        assert_eq!(bytes[4], VERSION);
        assert_eq!(bytes[5], MSG_STATUS);
        assert_eq!(bytes[6], 1);
        let session_id = u64::from_be_bytes([
            bytes[7], bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14],
        ]);
        assert_eq!(session_id, 0x1122_3344_5566_7788);
    }

    #[test]
    fn build_time_sync_response_contains_timestamps() {
        let packet = build_time_sync_response_packet(10, 20, 30);
        let bytes = packet.as_ref();
        assert_eq!(&bytes[0..4], MAGIC.as_slice());
        assert_eq!(bytes[4], VERSION);
        assert_eq!(bytes[5], MSG_TIME_SYNC_RESPONSE);
        assert_eq!(u64::from_be_bytes(bytes[6..14].try_into().unwrap()), 10);
        assert_eq!(u64::from_be_bytes(bytes[14..22].try_into().unwrap()), 20);
        assert_eq!(u64::from_be_bytes(bytes[22..30].try_into().unwrap()), 30);
    }

    #[test]
    fn build_media_control_packet_contains_command_and_argument() {
        let packet = build_media_control_packet(MediaCommand::VolumeDown, -1);
        let bytes = packet.as_ref();
        assert_eq!(&bytes[0..4], MAGIC.as_slice());
        assert_eq!(bytes[4], VERSION);
        assert_eq!(bytes[5], MSG_MEDIA_CONTROL);
        assert_eq!(bytes[6], MediaCommand::VolumeDown.code());
        assert_eq!(i64::from_be_bytes(bytes[7..15].try_into().unwrap()), -1);
    }

    #[test]
    fn build_now_playing_packet_contains_metadata() {
        let packet = build_now_playing_packet(
            42,
            &NowPlayingMetadata {
                artist: "Artist".to_string(),
                title: "Title".to_string(),
                playback_status: PlaybackStatus::Playing,
                position_ms: Some(12_345),
                duration_ms: Some(67_890),
                track_id: Some("/track/1".to_string()),
            },
        );
        let bytes = packet.as_ref();
        assert_eq!(&bytes[0..4], MAGIC.as_slice());
        assert_eq!(bytes[4], VERSION);
        assert_eq!(bytes[5], MSG_NOW_PLAYING);
        assert_eq!(u64::from_be_bytes(bytes[6..14].try_into().unwrap()), 42);
        assert_eq!(bytes[14], PlaybackStatus::Playing.code());
        assert_eq!(u16::from_be_bytes(bytes[15..17].try_into().unwrap()), 6);
        assert_eq!(u16::from_be_bytes(bytes[17..19].try_into().unwrap()), 5);
        assert_eq!(&bytes[19..25], b"Artist");
        assert_eq!(&bytes[25..30], b"Title");
        assert_eq!(u64::from_be_bytes(bytes[30..38].try_into().unwrap()), 12_345);
        assert_eq!(u64::from_be_bytes(bytes[38..46].try_into().unwrap()), 67_890);
    }
}
