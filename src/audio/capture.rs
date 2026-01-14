use libpulse_binding::mainloop::threaded::Mainloop;
use libpulse_binding::stream::Stream;

// `PcmFrame` represents one chunk of raw audio samples with a timestamp.
// A frame is just a convenient grouping so the rest of the program handles
// audio in manageable pieces instead of one sample at a time.
// The compiler ignores comments, so these notes are only for humans.
#[derive(Debug)]
pub struct PcmFrame {
    // `timestamp_ms` has type `u64`. It stores the time (in milliseconds) when
    // this frame was created. This is useful for syncing or ordering frames later.
    // `u64` means “unsigned 64‑bit integer,” which can store very large numbers.
    // Using milliseconds keeps the value easy to reason about for humans.
    pub timestamp_ms: u64,
    // `samples` has type `Vec<i16>`. It stores the actual audio sample values.
    // Each `i16` is a 16‑bit signed sample.
    // A `Vec` is a growable list stored on the heap, so it can hold many samples.
    pub samples: Vec<i16>,
}

// `RingBuffer` is a simple circular buffer used to collect samples into frames.
// It avoids reallocating a new buffer for every little chunk of audio.
// This is a common performance pattern in audio processing.
pub struct RingBuffer {
    // `data` holds the sample values in a fixed‑size vector.
    // The values stay in memory and are reused over and over.
    // This reduces memory churn and helps performance.
    data: Vec<i16>,
    // `capacity` is how many samples can fit in the buffer at once.
    // It is stored so the buffer knows when it is full.
    // `usize` is the natural size type for indexes on the current machine.
    capacity: usize,
    // `read` is the index where the next read will start.
    // It moves forward as samples are consumed.
    // The modulo operation wraps it around to the beginning.
    read: usize,
    // `write` is the index where the next write will happen.
    // It moves forward as samples are added.
    // This is the “tail” of the ring buffer.
    write: usize,
    // `len` is the number of samples currently stored.
    // Keeping a length avoids counting samples each time.
    // This is a standard approach in circular buffers.
    len: usize,
}

impl RingBuffer {
    // This creates a new ring buffer with a given capacity.
    // The buffer is initially filled with zeros.
    // The fields are set so reads and writes start at position 0.
    pub fn new(capacity: usize) -> Self {
        Self {
            data: vec![0; capacity],
            capacity,
            read: 0,
            write: 0,
            len: 0,
        }
    }

    // This adds a slice of samples to the ring buffer. If the buffer is full,
    // it discards the oldest samples so the newest data is kept.
    // This means the buffer always contains the most recent audio.
    // Dropping old samples is a deliberate choice to avoid unbounded growth.
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

    // This tries to remove a full frame of samples from the buffer. If there are
    // not enough samples yet, it returns `None` to indicate “not ready.”
    // `Option` is Rust’s way of representing “maybe there is a value, maybe not.”
    // Returning `None` lets the caller try again later without error.
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

// `PulseStreamHandle` wraps a PulseAudio stream along with its mainloop pointer.
// This makes it easier to shut the stream down safely later.
// The fields are private so external code cannot misuse them.
pub struct PulseStreamHandle {
    // `stream` has type `Box<Stream>`. The `Box` means it is stored on the heap,
    // which is needed because we later take raw pointers to it.
    // Heap allocation keeps the object at a stable memory address.
    // Raw pointers require that stability to remain safe.
    stream: Box<Stream>,
    // `mainloop` is a raw pointer to the PulseAudio mainloop, required so we can
    // lock and unlock it inside unsafe callbacks.
    // Raw pointers are unsafe to dereference, so they are used carefully here.
    // This pointer is only used inside controlled `unsafe` blocks.
    mainloop: *const Mainloop,
    // `closed` is a simple guard to prevent shutting down the stream twice.
    // This avoids double‑free style bugs and keeps cleanup idempotent.
    // A boolean flag is often the simplest way to protect against repeated cleanup.
    closed: bool,
}

impl PulseStreamHandle {
    // This creates a new handle around a PulseAudio stream and remembers the
    // mainloop so the stream can be properly shut down.
    // The `new` method is an associated function, meaning it is called on the type.
    // It returns a fully initialized handle ready for use.
    pub fn new(stream: Stream, mainloop: &Mainloop) -> Self {
        Self {
            stream: Box::new(stream),
            mainloop,
            closed: false,
        }
    }

    // This returns a raw pointer to the underlying stream. Raw pointers are used
    // because the PulseAudio C API expects them, even though they are unsafe.
    // Returning a pointer does not transfer ownership; it is just a view.
    // The caller must ensure the stream outlives any pointer usage.
    pub fn as_ptr(&mut self) -> *mut Stream {
        &mut *self.stream
    }

    // This shuts down the stream by removing callbacks and disconnecting it.
    // The mainloop is locked while this happens so PulseAudio stays consistent.
    // The `unsafe` block is needed because raw pointers are being dereferenced.
    // Rust forces you to mark such code explicitly to highlight risk.
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

// This ensures that if a handle is dropped, the stream is still shut down.
// The `Drop` trait is Rust’s equivalent of a destructor.
// This is a safety net so cleanup happens even if the caller forgets.
impl Drop for PulseStreamHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// `CaptureSession` is a small wrapper used to represent an active capture stream.
// It exists to make it clear when a capture is considered “alive.”
// This type is not exposed publicly outside the module.
pub struct CaptureSession {
    // `handle` owns the PulseAudio stream so it stays alive while capturing.
    // Ownership is important in Rust; when this handle is dropped, cleanup happens.
    // That gives a clear lifecycle for the capture session.
    handle: PulseStreamHandle,
}

impl CaptureSession {
    // This creates a new capture session around a handle.
    // The `pub(crate)` visibility means it is public only within this crate.
    // That keeps the API tight and prevents misuse from other crates.
    pub(crate) fn new(handle: PulseStreamHandle) -> Self {
        Self { handle }
    }

    // This shuts down the underlying PulseAudio stream.
    // It forwards directly to the handle’s shutdown method.
    // This keeps the capture session’s public behavior simple.
    pub(crate) fn shutdown(&mut self) {
        self.handle.shutdown();
    }
}
