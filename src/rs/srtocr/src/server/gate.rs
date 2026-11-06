//! Serialisation of every inference call in the process.
//!
//! A local llama.cpp backend holds a single model in VRAM. Two requests that
//! overlap do not finish any sooner; they split the same compute budget and
//! can make both miss their timeout. Every path that talks to the model - the
//! job worker and the frame-stream handler - takes this gate first, so at most
//! one inference is in flight no matter how many clients are connected.

use std::sync::{Arc, Condvar, Mutex, MutexGuard};

/// Number of inference calls allowed to the backend at once.
///
/// One on purpose: the VRAM budget fits one model, and queuing the rest is
/// what keeps latency predictable. Raising this without a second model
/// resident would only interleave the same requests.
pub const MAX_CONCURRENT_INFERENCE: usize = 1;

#[derive(Debug, Default)]
struct Inner {
    in_flight: usize,
}

/// Shared gate. Cheap to clone; every clone points at the same counter.
#[derive(Debug, Clone)]
pub struct InferenceGate {
    inner: Arc<(Mutex<Inner>, Condvar)>,
    limit: usize,
}

impl Default for InferenceGate {
    fn default() -> Self {
        Self::new(MAX_CONCURRENT_INFERENCE)
    }
}

impl InferenceGate {
    /// Build a gate with an explicit limit. Tests use a larger limit to prove
    /// the counting itself works; production callers use `default`.
    pub fn new(limit: usize) -> Self {
        assert!(limit > 0, "inference gate needs at least one permit");
        Self {
            inner: Arc::new((Mutex::new(Inner { in_flight: 0 }), Condvar::new())),
            limit,
        }
    }

    /// Block until a permit is free, then take it. The returned guard releases
    /// the permit on drop, including when the guarded call panics.
    pub fn acquire(&self) -> Permit<'_> {
        let (lock, cvar) = &*self.inner;
        let mut inner = self.lock(lock);
        while inner.in_flight >= self.limit {
            inner = cvar.wait(inner).unwrap_or_else(|e| e.into_inner());
        }
        inner.in_flight += 1;
        Permit { gate: self }
    }

    /// Number of calls currently holding a permit. Diagnostics and tests only.
    pub fn in_flight(&self) -> usize {
        let (lock, _) = &*self.inner;
        self.lock(lock).in_flight
    }

    /// Configured ceiling. Diagnostics and tests only.
    pub fn limit(&self) -> usize {
        self.limit
    }

    fn release(&self) {
        let (lock, cvar) = &*self.inner;
        let mut inner = self.lock(lock);
        inner.in_flight -= 1;
        cvar.notify_one();
    }

    fn lock<'a>(&self, m: &'a Mutex<Inner>) -> MutexGuard<'a, Inner> {
        // A panicking holder must not wedge every later caller.
        m.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Held for the duration of one inference call.
#[derive(Debug)]
pub struct Permit<'a> {
    gate: &'a InferenceGate,
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        self.gate.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn single_permit_serialises_callers() {
        let gate = InferenceGate::default();
        assert_eq!(gate.limit(), MAX_CONCURRENT_INFERENCE);
        let peak = Arc::new(AtomicUsize::new(0));
        let running = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..4 {
            let gate = gate.clone();
            let peak = Arc::clone(&peak);
            let running = Arc::clone(&running);
            handles.push(std::thread::spawn(move || {
                let _permit = gate.acquire();
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                // Long enough that an overlap would be observed if the gate
                // were not doing its job.
                std::thread::sleep(Duration::from_millis(20));
                running.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(peak.load(Ordering::SeqCst), 1, "two callers overlapped");
        assert_eq!(gate.in_flight(), 0);
    }

    #[test]
    fn permit_is_released_when_the_guarded_call_panics() {
        let gate = InferenceGate::default();
        let gate_for_thread = gate.clone();
        let _ = std::thread::spawn(move || {
            let _permit = gate_for_thread.acquire();
            panic!("inference blew up");
        })
        .join();
        // The panic unwound through Drop; the next caller must not block.
        let _permit = gate.acquire();
        assert_eq!(gate.in_flight(), 1);
    }

    #[test]
    fn extra_permits_are_granted_up_to_the_limit() {
        let gate = InferenceGate::new(2);
        let a = gate.acquire();
        let b = gate.acquire();
        assert_eq!(gate.in_flight(), 2);
        drop(a);
        assert_eq!(gate.in_flight(), 1);
        drop(b);
        assert_eq!(gate.in_flight(), 0);
    }
}
