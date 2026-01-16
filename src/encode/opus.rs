use crate::control::messages::StreamConfig;
use opus::{Application, Channels, Encoder};

const MAX_PACKET_SIZE: usize = 4000;

pub struct OpusEncoder {
    encoder: Encoder,
    max_packet_size: usize,
}

impl OpusEncoder {
    pub fn new(config: StreamConfig) -> Result<Self, opus::Error> {
        let channels = match config.channels {
            1 => Channels::Mono,
            _ => Channels::Stereo,
        };

        let encoder = Encoder::new(config.sample_rate, channels, Application::Audio)?;
        Ok(Self {
            encoder,
            max_packet_size: MAX_PACKET_SIZE,
        })
    }

    pub fn encode_frame(&mut self, pcm: &[i16]) -> Result<Vec<u8>, opus::Error> {
        let mut buffer = vec![0u8; self.max_packet_size];
        let len = self.encoder.encode(pcm, &mut buffer)?;
        buffer.truncate(len);
        Ok(buffer)
    }
}