use crate::control::messages::StreamConfig;
use opus::{Application, Channels, Encoder};

// `MAX_PACKET_SIZE` is a simple safety limit for how large an encoded packet
// can be. It helps us pre‑allocate a buffer big enough for most audio frames.
// Pre‑allocating avoids repeated memory allocations inside the hot path.
// The exact size is a practical choice rather than a strict rule.
const MAX_PACKET_SIZE: usize = 4000;

// `OpusEncoder` wraps the opus library encoder so the rest of the program can
// call simple methods without worrying about the lower‑level API details.
// This is a small “adapter” type that hides library complexity.
// Wrapping third‑party APIs like this is a common Rust design pattern.
pub struct OpusEncoder {
    // `encoder` is the actual opus encoder from the external library.
    // It owns any internal state needed by the codec.
    // Keeping it as a field means it stays alive between calls.
    encoder: Encoder,
    // `max_packet_size` is a copy of the constant used to size the buffer.
    // It is stored so `encode_frame` doesn’t need to refer to the constant directly.
    // This also makes testing easier if you ever want to change the size.
    max_packet_size: usize,
}

impl OpusEncoder {
    // This constructs an Opus encoder based on the stream configuration.
    // It chooses mono or stereo based on the channel count.
    // Any error from the opus library is returned to the caller.
    pub fn new(config: StreamConfig) -> Result<Self, opus::Error> {
        // `channels` converts a numeric channel count into the opus enum.
        // The match expression is Rust’s way of branching on a value.
        // This keeps the code explicit about mono vs stereo.
        let channels = match config.channels {
            1 => Channels::Mono,
            _ => Channels::Stereo,
        };

        // `encoder` is created with the sample rate and channel mode.
        // `Application::Audio` tells opus to optimize for general audio.
        // The `?` operator forwards errors to the caller if creation fails.
        let encoder = Encoder::new(config.sample_rate, channels, Application::Audio)?;
        Ok(Self {
            encoder,
            max_packet_size: MAX_PACKET_SIZE,
        })
    }

    // This encodes a single PCM frame into an Opus packet.
    // The PCM data must match the format described by `StreamConfig`.
    // The returned `Vec<u8>` contains just the encoded bytes.
    pub fn encode_frame(&mut self, pcm: &[i16]) -> Result<Vec<u8>, opus::Error> {
        // `buffer` is a byte vector filled with zeros and large enough to hold
        // the encoded packet. The encoder writes into this buffer.
        // Pre‑allocating like this avoids resizing during encoding.
        // The buffer lives on the heap because `Vec` stores data there.
        let mut buffer = vec![0u8; self.max_packet_size];
        // `len` is the number of bytes the encoder actually produced.
        // The encoder returns this length so we can trim the buffer.
        // A shorter packet is normal; not all frames compress to the same size.
        let len = self.encoder.encode(pcm, &mut buffer)?;
        // `truncate` removes unused bytes so the returned vector is exact.
        // This keeps packet sizes accurate and prevents sending extra zeros.
        // It also makes downstream code simpler because it gets the right length.
        buffer.truncate(len);
        Ok(buffer)
    }
}
