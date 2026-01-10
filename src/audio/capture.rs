use libpulse_binding::stream::Stream;

#[derive(Debug)]
pub struct PcmFrame {
    pub timestamp_ms: u64,
    pub samples: Vec<i16>,
}

pub struct CaptureSession {
    stream: Option<Box<Stream>>,
}

impl CaptureSession {
    pub(crate) fn new(stream: Stream) -> Self {
        Self {
            stream: Some(Box::new(stream)),
        }
    }

    pub(crate) fn as_ptr(&self) -> *mut Stream {
        self.stream
            .as_ref()
            .map(|stream| &**stream as *const Stream as *mut Stream)
            .expect("capture stream missing")
    }

    pub(crate) fn disconnect(&mut self) {
        if let Some(stream) = self.stream.as_mut() {
            stream.disconnect().ok();
        }
    }
}

impl Drop for CaptureSession {
    fn drop(&mut self) {
        if let Some(stream) = self.stream.as_mut() {
            stream.disconnect().ok();
        }
    }
}
