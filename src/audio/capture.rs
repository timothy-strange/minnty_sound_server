use libpulse_binding::mainloop::threaded::Mainloop;
use libpulse_binding::stream::Stream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

#[derive(Debug)]
pub struct PcmFrame {
    pub timestamp_ms: u64,
    pub samples: Vec<i16>,
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
    stop_flag: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl CaptureSession {
    pub(crate) fn new(
        handle: PulseStreamHandle,
        stop_flag: Arc<AtomicBool>,
        worker: JoinHandle<()>,
    ) -> Self {
        Self {
            handle,
            stop_flag,
            worker: Some(worker),
        }
    }

    pub(crate) fn shutdown(&mut self) {
        self.stop_flag.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.handle.shutdown();
    }
}
