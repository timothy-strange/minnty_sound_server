use libpulse_binding::mainloop::threaded::Mainloop;
use libpulse_binding::stream::Stream;

#[derive(Debug)]
pub struct PcmFrame {
    pub timestamp_ms: u64,
    pub samples: Vec<i16>,
}

pub struct RingBuffer {
    data: Vec<i16>,
    capacity: usize,
    read: usize,
    write: usize,
    len: usize,
}

impl RingBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            data: vec![0; capacity],
            capacity,
            read: 0,
            write: 0,
            len: 0,
        }
    }

    pub fn push_samples(&mut self, samples: &[i16]) {
        for &sample in samples {
            if self.len == self.capacity {
                self.read = (self.read + 1) % self.capacity;
                self.len -= 1;
            }
            self.data[self.write] = sample;
            self.write = (self.write + 1) % self.capacity;
            self.len += 1;
        }
    }

    pub fn pop_frame(&mut self, frame_size: usize) -> Option<Vec<i16>> {
        if self.len < frame_size {
            return None;
        }

        let mut frame = Vec::with_capacity(frame_size);
        for _ in 0..frame_size {
            frame.push(self.data[self.read]);
            self.read = (self.read + 1) % self.capacity;
            self.len -= 1;
        }
        Some(frame)
    }
}

pub struct PulseStreamHandle {
    stream: Box<Stream>,
    mainloop: *const Mainloop,
    closed: bool,
}

impl PulseStreamHandle {
    pub fn new(stream: Stream, mainloop: &Mainloop) -> Self {
        Self {
            stream: Box::new(stream),
            mainloop,
            closed: false,
        }
    }

    pub fn as_ptr(&mut self) -> *mut Stream {
        &mut *self.stream
    }

    pub fn shutdown(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        unsafe {
            let mainloop = &mut *(self.mainloop as *mut Mainloop);
            mainloop.lock();
            self.stream.set_read_callback(None);
            self.stream.disconnect().ok();
            mainloop.unlock();
        }
    }
}

impl Drop for PulseStreamHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub struct CaptureSession {
    handle: PulseStreamHandle,
}

impl CaptureSession {
    pub(crate) fn new(handle: PulseStreamHandle) -> Self {
        Self { handle }
    }

    pub(crate) fn shutdown(&mut self) {
        self.handle.shutdown();
    }
}