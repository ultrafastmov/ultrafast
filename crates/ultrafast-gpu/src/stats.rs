use std::sync::atomic::{AtomicU64, Ordering};

/// Cumulative CPU<->GPU transfer counters. Uniform params bound inline with a
/// dispatch are counted separately from bulk uploads, since they are the
/// expected steady-state cost of a frame.
#[derive(Debug, Default)]
pub(crate) struct Counters {
    pub uploads: AtomicU64,
    pub upload_bytes: AtomicU64,
    pub readbacks: AtomicU64,
    pub readback_bytes: AtomicU64,
    pub uniform_bytes: AtomicU64,
    pub textures_created: AtomicU64,
    pub dispatches: AtomicU64,
    /// GPU-wide barriers recorded (Vulkan: only where commands depend).
    pub barriers: AtomicU64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TransferStats {
    pub uploads: u64,
    pub upload_bytes: u64,
    pub readbacks: u64,
    pub readback_bytes: u64,
    pub uniform_bytes: u64,
    pub textures_created: u64,
    pub dispatches: u64,
    pub barriers: u64,
}

impl Counters {
    pub fn snapshot(&self) -> TransferStats {
        let l = |a: &AtomicU64| a.load(Ordering::Relaxed);
        TransferStats {
            uploads: l(&self.uploads),
            upload_bytes: l(&self.upload_bytes),
            readbacks: l(&self.readbacks),
            readback_bytes: l(&self.readback_bytes),
            uniform_bytes: l(&self.uniform_bytes),
            textures_created: l(&self.textures_created),
            dispatches: l(&self.dispatches),
            barriers: l(&self.barriers),
        }
    }

    pub fn add(a: &AtomicU64, n: u64) {
        a.fetch_add(n, Ordering::Relaxed);
    }
}

impl std::ops::Sub for TransferStats {
    type Output = TransferStats;
    fn sub(self, o: Self) -> Self {
        TransferStats {
            uploads: self.uploads - o.uploads,
            upload_bytes: self.upload_bytes - o.upload_bytes,
            readbacks: self.readbacks - o.readbacks,
            readback_bytes: self.readback_bytes - o.readback_bytes,
            uniform_bytes: self.uniform_bytes - o.uniform_bytes,
            textures_created: self.textures_created - o.textures_created,
            dispatches: self.dispatches - o.dispatches,
            barriers: self.barriers - o.barriers,
        }
    }
}
