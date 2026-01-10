use libpulse_binding::stream::Stream;
use std::ptr::NonNull;

#[derive(Debug)]
pub struct PcmFrame {
    pub timestamp_ms: u64,
    pub samples: Vec<i16>,
}

pub struct CaptureSession {
    stream: Option<NonNull<Stream>>,
}

impl CaptureSession {
    pub(crate) fn new(stream: Stream) -> Self {
        let stream = Box::into_raw(Box::new(stream));
        let stream = unsafe { NonNull::new_unchecked(stream) };
        Self {
            stream: Some(stream),
        }
    }

    pub(crate) fn as_ptr(&self) -> *mut Stream {
        self.stream.expect("capture stream missing").as_ptr()
    }

    pub(crate) fn take_stream(mut self) -> Option<NonNull<Stream>> {
        self.stream.take()
    }
}

impl Drop for CaptureSession {
    fn drop(&mut self) {
        if let Some(stream) = self.stream.take() {
            unsafe {
                let stream = stream.as_ptr();
                let mut boxed = Box::from_raw(stream);
                boxed.disconnect().ok();
            }
        }
    }
}
