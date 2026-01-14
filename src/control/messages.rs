use bytes::{BufMut, Bytes, BytesMut};

// `MAGIC` is a 4‑byte marker that tells us a packet belongs to this protocol.
// It is used like a signature at the front of each control packet.
// Using a magic marker helps reject random network traffic.
// It is a simple but effective validation step.
pub const MAGIC: [u8; 4] = *b"MNTY";
// `VERSION` is a single byte that lets clients and servers agree on format.
// If versions differ, the packet is ignored.
// This makes it easier to change the protocol later without breaking old clients.
// Versioning is a common practice in network protocols.
pub const VERSION: u8 = 1;

// These constants identify different control packet types.
// Using named constants makes the code easier to read than raw numbers.
// The numbers themselves are part of the protocol definition.
// Both server and client must agree on these values.
pub const MSG_HELLO: u8 = 1;
pub const MSG_CONFIG: u8 = 2;
pub const MSG_KEEPALIVE: u8 = 3;

// `StreamConfig` stores the audio settings shared between server and client.
// It acts like a small “settings card” for streaming.
// The `derive` line tells Rust to auto‑generate helpers like `Copy` and `Debug`.
// Those helpers make it easier to pass and print the config.
#[derive(Clone, Copy, Debug)]
pub struct StreamConfig {
    // `udp_port` is the UDP port number the server uses for streaming.
    // Ports are like numbered mailboxes on a machine.
    // Clients must send packets to this port to reach the stream.
    pub udp_port: u16,
    // `sample_rate` is the number of samples per second (e.g. 48000).
    // A higher sample rate captures more detail but uses more bandwidth.
    // This is a common audio setting used across many systems.
    pub sample_rate: u32,
    // `channels` is 1 for mono or 2 for stereo.
    // Stereo has two channels, usually left and right.
    // This affects how many samples are produced per frame.
    pub channels: u8,
    // `frame_size` is the number of samples per channel in each frame.
    // Smaller frames mean lower latency but more packets.
    // Larger frames mean fewer packets but more delay.
    pub frame_size: usize,
    // `pcm_queue_depth` is the maximum number of PCM frames buffered in memory.
    // This smooths out small delays in processing.
    // The depth is a trade‑off between memory use and resilience to hiccups.
    pub pcm_queue_depth: usize,
}

// This is the default configuration used when the program starts.
// Using a constant keeps the startup path simple and predictable.
// These values can be changed later if needed.
// Defaults are helpful for first‑time users.
pub const DEFAULT_STREAM_CONFIG: StreamConfig = StreamConfig {
    udp_port: 40110,
    sample_rate: 48_000,
    channels: 2,
    frame_size: 960,
    pcm_queue_depth: 16,
};

// `ControlMessage` is a small enum that represents client control messages.
// Enums in Rust list possible variants in a type‑safe way.
// This avoids using raw numbers everywhere in code.
// It also makes pattern matching more readable.
#[derive(Clone, Copy, Debug)]
pub enum ControlMessage {
    // `Hello` means “I am a client and I want to connect.”
    // This is typically the first packet a client sends.
    // The server replies with configuration information.
    Hello,
    // `KeepAlive` means “I am still here; don’t forget me.”
    // This is sent periodically so the server doesn’t time out the client.
    // It is a simple way to keep the connection fresh in UDP.
    KeepAlive,
}

// This reads a raw packet and returns a `ControlMessage` if it is valid.
// The return type is `Option`, so `None` means “not a valid control packet.”
// This function is defensive: it checks length, magic bytes, and version.
// That prevents malformed packets from confusing the server.
pub fn parse_control_packet(data: &[u8]) -> Option<ControlMessage> {
    // `data` is a slice of bytes. If it is too short, it cannot be valid.
    // A slice is a read‑only view into an array or vector.
    // Checking length first avoids panics from out‑of‑bounds indexing.
    if data.len() < 6 {
        return None;
    }

    // The first four bytes must match the MAGIC signature.
    // This is like checking the “header” of a letter.
    // If it doesn’t match, the packet is ignored.
    if data[0..4] != MAGIC {
        return None;
    }

    // The fifth byte must match the protocol version.
    // This ensures both sides are speaking the same format.
    // It also allows future changes without breaking older clients.
    if data[4] != VERSION {
        return None;
    }

    // The sixth byte decides which control message this is.
    // Pattern matching is a Rust feature that cleanly handles multiple cases.
    // Any unknown value results in `None`.
    match data[5] {
        MSG_HELLO => Some(ControlMessage::Hello),
        MSG_KEEPALIVE => Some(ControlMessage::KeepAlive),
        _ => None,
    }
}

// This builds a packet that tells clients the stream configuration.
// The packet format is a sequence of bytes in a known order.
// `Bytes` is a compact, shared byte buffer type from the `bytes` crate.
// It is commonly used for network protocols.
pub fn build_config_packet(config: StreamConfig) -> Bytes {
    // `buf` is a growable byte buffer used to assemble the packet.
    // `BytesMut` is mutable, allowing us to append data efficiently.
    // We give it a capacity so it doesn’t need to resize while building.
    let mut buf = BytesMut::with_capacity(4 + 1 + 1 + 2 + 4 + 1 + 2);
    // Each `put_*` call appends data in the correct order.
    // The order matters because the client expects fields in this sequence.
    // These functions write values in big‑endian network order.
    buf.put_slice(&MAGIC);
    buf.put_u8(VERSION);
    buf.put_u8(MSG_CONFIG);
    buf.put_u16(config.udp_port);
    buf.put_u32(config.sample_rate);
    buf.put_u8(config.channels);
    buf.put_u16(config.frame_size as u16);
    // `freeze` turns the mutable buffer into an immutable `Bytes` object.
    // This makes it cheap to clone and send without copying the data.
    // Immutability helps avoid accidental modifications later.
    buf.freeze()
}

// This builds a packet that carries encoded audio data.
// It adds a small header so the client can interpret the payload.
// The payload itself is the compressed Opus audio bytes.
// This function does not send the packet; it only constructs it.
pub fn build_stream_packet(seq: u32, timestamp_ms: u64, payload: &[u8]) -> Bytes {
    // `buf` is a mutable byte buffer sized for header + payload.
    // The size calculation ensures enough space for all fields.
    // Pre‑allocating improves performance under load.
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
        // `hello` is a fake Hello packet built for testing the parser.
        // Tests like this make sure the parser behaves as expected.
        // In Rust, tests are just functions annotated with `#[test]`.
        let mut hello = Vec::new();
        hello.extend_from_slice(&MAGIC);
        hello.push(VERSION);
        hello.push(MSG_HELLO);
        assert!(matches!(
            parse_control_packet(&hello),
            Some(ControlMessage::Hello)
        ));

        // `keepalive` is a fake KeepAlive packet.
        // It uses the same header but a different message type.
        // This ensures the parser distinguishes the two.
        let mut keepalive = Vec::new();
        keepalive.extend_from_slice(&MAGIC);
        keepalive.push(VERSION);
        keepalive.push(MSG_KEEPALIVE);
        assert!(matches!(
            parse_control_packet(&keepalive),
            Some(ControlMessage::KeepAlive)
        ));

        // `invalid` uses the wrong magic bytes, so parsing should fail.
        // This confirms the parser rejects packets from other protocols.
        // Defensive checks like this help avoid subtle bugs.
        let mut invalid = Vec::new();
        invalid.extend_from_slice(b"NOPE");
        invalid.push(VERSION);
        invalid.push(MSG_HELLO);
        assert!(parse_control_packet(&invalid).is_none());
    }

    #[test]
    fn build_config_packet_contains_fields() {
        // `packet` is a serialized config packet for the default settings.
        // We inspect its bytes to make sure the header is correct.
        // This test focuses on the magic bytes and version.
        let packet = build_config_packet(DEFAULT_STREAM_CONFIG);
        // `bytes` is a raw view into the packet for inspection.
        // It is a slice, which is a lightweight reference to the data.
        // Slices are commonly used when you only need to read data.
        let bytes = packet.as_ref();
        assert_eq!(&bytes[0..4], MAGIC.as_slice());
        assert_eq!(bytes[4], VERSION);
        assert_eq!(bytes[5], MSG_CONFIG);
    }

    #[test]
    fn build_stream_packet_has_payload_length() {
        // `payload` represents fake encoded audio bytes.
        // Using a small array keeps the test simple and easy to reason about.
        // In real usage, payloads are much larger.
        let payload = [1u8, 2, 3, 4, 5];
        // `packet` is a full stream packet built from the payload.
        // The header fields should match what we passed in.
        // This test specifically checks the payload length field.
        let packet = build_stream_packet(42, 1000, &payload);
        // `bytes` is a raw slice view into the packet.
        // This makes it easy to read header fields by index.
        // It is a common technique in low‑level protocol tests.
        let bytes = packet.as_ref();
        assert_eq!(bytes[0], VERSION);
        let len = u16::from_be_bytes([bytes[13], bytes[14]]);
        assert_eq!(len as usize, payload.len());
    }
}
