use std::sync::Mutex;

/// Fixed-capacity circular buffer of interleaved f32 samples. Pushing past
/// capacity silently drops the oldest samples, which is exactly the "last N
/// seconds" behavior we want.
pub struct RingBuffer {
    inner: Mutex<Inner>,
}

struct Inner {
    data: Vec<f32>,
    write_pos: usize,
    filled: usize,
}

impl Inner {
    /// Writes `n` samples produced by `fill(offset, dst)` in at most two
    /// contiguous chunks.
    fn write_with(&mut self, n: usize, mut fill: impl FnMut(usize, &mut [f32])) {
        let cap = self.data.len();
        if cap == 0 || n == 0 {
            return;
        }
        // Only the newest `cap` samples can survive.
        let skip = n.saturating_sub(cap);
        let mut done = skip;
        while done < n {
            let chunk = (n - done).min(cap - self.write_pos);
            let wp = self.write_pos;
            fill(done, &mut self.data[wp..wp + chunk]);
            self.write_pos = (wp + chunk) % cap;
            done += chunk;
        }
        self.filled = (self.filled + n).min(cap);
    }
}

impl RingBuffer {
    pub fn new(capacity_samples: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                data: vec![0.0; capacity_samples],
                write_pos: 0,
                filled: 0,
            }),
        }
    }

    pub fn push(&self, samples: &[f32]) {
        self.inner
            .lock()
            .unwrap()
            .write_with(samples.len(), |off, dst| {
                dst.copy_from_slice(&samples[off..off + dst.len()])
            });
    }

    pub fn push_silence(&self, n: usize) {
        self.inner
            .lock()
            .unwrap()
            .write_with(n, |_, dst| dst.fill(0.0));
    }

    /// Returns the buffered samples in chronological order (oldest first).
    pub fn snapshot(&self) -> Vec<f32> {
        let inner = self.inner.lock().unwrap();
        let cap = inner.data.len();
        if inner.filled < cap {
            return inner.data[..inner.filled].to_vec();
        }
        let mut out = Vec::with_capacity(cap);
        out.extend_from_slice(&inner.data[inner.write_pos..]);
        out.extend_from_slice(&inner.data[..inner.write_pos]);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_fill() {
        let rb = RingBuffer::new(5);
        rb.push(&[1.0, 2.0]);
        assert_eq!(rb.snapshot(), vec![1.0, 2.0]);
    }

    #[test]
    fn wraps_and_keeps_newest() {
        let rb = RingBuffer::new(4);
        rb.push(&[1.0, 2.0, 3.0]);
        rb.push(&[4.0, 5.0, 6.0]);
        assert_eq!(rb.snapshot(), vec![3.0, 4.0, 5.0, 6.0]);
    }

    #[test]
    fn push_larger_than_capacity() {
        let rb = RingBuffer::new(3);
        rb.push(&[1.0]);
        rb.push(&[2.0, 3.0, 4.0, 5.0, 6.0]);
        assert_eq!(rb.snapshot(), vec![4.0, 5.0, 6.0]);
    }

    #[test]
    fn silence() {
        let rb = RingBuffer::new(4);
        rb.push(&[1.0, 2.0, 3.0]);
        rb.push_silence(2);
        assert_eq!(rb.snapshot(), vec![2.0, 3.0, 0.0, 0.0]);
    }
}
