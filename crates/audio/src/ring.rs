//! A lock-free ring of f32 samples with one producer and one consumer.
//!
//! Both ends are held by audio callbacks, so nothing here allocates, locks or
//! blocks. A push into a full ring and a pop from an empty one give up and
//! count the samples they could not move, because stalling an IO thread costs
//! far more than the audio it would save.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct Ring {
    data: *mut f32,
    capacity: usize,
    read: AtomicUsize,
    write: AtomicUsize,
    lost: AtomicUsize,
    producer_taken: AtomicBool,
    consumer_taken: AtomicBool,
}

// The two ends only ever touch disjoint regions of `data`, and which region is
// theirs is settled by the read and write counters.
unsafe impl Send for Ring {}
unsafe impl Sync for Ring {}

impl Drop for Ring {
    fn drop(&mut self) {
        unsafe { drop(Vec::from_raw_parts(self.data, 0, self.capacity)) };
    }
}

impl Ring {
    fn len(&self) -> usize {
        self.write
            .load(Ordering::Acquire)
            .wrapping_sub(self.read.load(Ordering::Acquire))
    }
}

/// Creates a ring holding `capacity` samples, and hands out its two ends.
pub fn ring(capacity: usize) -> Arc<Shared> {
    let mut data = Vec::<f32>::with_capacity(capacity);
    let ptr = data.as_mut_ptr();
    std::mem::forget(data);
    Arc::new(Shared(Ring {
        data: ptr,
        capacity,
        read: AtomicUsize::new(0),
        write: AtomicUsize::new(0),
        lost: AtomicUsize::new(0),
        producer_taken: AtomicBool::new(false),
        consumer_taken: AtomicBool::new(false),
    }))
}

/// The ring itself, from which each end can be taken exactly once at a time.
pub struct Shared(Ring);

impl Shared {
    /// The writing end, or `None` while one is already out. Handing out a
    /// second producer would break the single-writer rule the ring is built on.
    pub fn producer(self: &Arc<Self>) -> Option<Producer> {
        (!self.0.producer_taken.swap(true, Ordering::AcqRel))
            .then(|| Producer { ring: self.clone() })
    }

    pub fn consumer(self: &Arc<Self>) -> Option<Consumer> {
        (!self.0.consumer_taken.swap(true, Ordering::AcqRel))
            .then(|| Consumer { ring: self.clone() })
    }

    pub fn lost(&self) -> usize {
        self.0.lost.load(Ordering::Relaxed)
    }

    /// Throws away everything queued. Only safe between runs, when neither end
    /// is in a callback.
    pub fn clear(&self) {
        self.0
            .read
            .store(self.0.write.load(Ordering::Acquire), Ordering::Release);
    }
}

pub struct Producer {
    ring: Arc<Shared>,
}

impl Drop for Producer {
    fn drop(&mut self) {
        self.ring.0.producer_taken.store(false, Ordering::Release);
    }
}

impl Producer {
    /// Writes what fits and returns how much that was.
    pub fn push(&mut self, src: &[f32]) -> usize {
        let ring = &self.ring.0;
        let write = ring.write.load(Ordering::Relaxed);
        let free = ring.capacity - write.wrapping_sub(ring.read.load(Ordering::Acquire));
        let n = src.len().min(free);

        let start = write % ring.capacity;
        let first = n.min(ring.capacity - start);
        unsafe {
            std::ptr::copy_nonoverlapping(src.as_ptr(), ring.data.add(start), first);
            std::ptr::copy_nonoverlapping(src.as_ptr().add(first), ring.data, n - first);
        }
        ring.write.store(write.wrapping_add(n), Ordering::Release);

        if n < src.len() {
            ring.lost.fetch_add(src.len() - n, Ordering::Relaxed);
        }
        n
    }
}

pub struct Consumer {
    ring: Arc<Shared>,
}

impl Drop for Consumer {
    fn drop(&mut self) {
        self.ring.0.consumer_taken.store(false, Ordering::Release);
    }
}

impl Consumer {
    /// Reads what is there and returns how much that was. The rest of `dst` is
    /// left untouched.
    pub fn pop(&mut self, dst: &mut [f32]) -> usize {
        let ring = &self.ring.0;
        let read = ring.read.load(Ordering::Relaxed);
        let available = ring.write.load(Ordering::Acquire).wrapping_sub(read);
        let n = dst.len().min(available);

        let start = read % ring.capacity;
        let first = n.min(ring.capacity - start);
        unsafe {
            std::ptr::copy_nonoverlapping(ring.data.add(start), dst.as_mut_ptr(), first);
            std::ptr::copy_nonoverlapping(ring.data, dst.as_mut_ptr().add(first), n - first);
        }
        ring.read.store(read.wrapping_add(n), Ordering::Release);
        n
    }

    /// Throws away the oldest samples, returning how many went. Used to drop
    /// a backlog in one go rather than playing it out.
    pub fn skip(&mut self, samples: usize) -> usize {
        let ring = &self.ring.0;
        let read = ring.read.load(Ordering::Relaxed);
        let n = samples.min(ring.write.load(Ordering::Acquire).wrapping_sub(read));
        ring.read.store(read.wrapping_add(n), Ordering::Release);
        n
    }

    pub fn len(&self) -> usize {
        self.ring.0.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_goes_in_comes_out_in_order() {
        let shared = ring(16);
        let mut producer = shared.producer().unwrap();
        let mut consumer = shared.consumer().unwrap();

        assert_eq!(producer.push(&[1.0, 2.0, 3.0]), 3);
        let mut out = [0.0; 3];
        assert_eq!(consumer.pop(&mut out), 3);
        assert_eq!(out, [1.0, 2.0, 3.0]);
        assert_eq!(consumer.len(), 0);
    }

    #[test]
    fn it_wraps_without_losing_a_sample() {
        let shared = ring(8);
        let mut producer = shared.producer().unwrap();
        let mut consumer = shared.consumer().unwrap();

        let mut expected = 0.0;
        let mut out = [0.0; 5];
        for round in 0..20 {
            let block: Vec<f32> = (0..5).map(|i| (round * 5 + i) as f32).collect();
            assert_eq!(producer.push(&block), 5);
            assert_eq!(consumer.pop(&mut out), 5);
            for sample in out {
                assert_eq!(sample, expected);
                expected += 1.0;
            }
        }
    }

    #[test]
    fn a_full_ring_drops_the_newest_and_counts_it() {
        let shared = ring(4);
        let mut producer = shared.producer().unwrap();
        assert_eq!(producer.push(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]), 4);
        assert_eq!(shared.lost(), 2);

        let mut consumer = shared.consumer().unwrap();
        let mut out = [0.0; 4];
        assert_eq!(consumer.pop(&mut out), 4);
        assert_eq!(out, [1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn an_empty_ring_leaves_the_destination_alone() {
        let shared = ring(4);
        let mut consumer = shared.consumer().unwrap();
        let mut out = [9.0; 3];
        assert_eq!(consumer.pop(&mut out), 0);
        assert_eq!(out, [9.0; 3]);
    }

    #[test]
    fn only_one_end_of_each_kind_is_ever_out() {
        let shared = ring(4);
        let producer = shared.producer().unwrap();
        assert!(shared.producer().is_none());
        drop(producer);
        assert!(shared.producer().is_some());
    }

    #[test]
    fn clearing_drops_everything_queued() {
        let shared = ring(8);
        let mut producer = shared.producer().unwrap();
        let consumer = shared.consumer().unwrap();
        producer.push(&[1.0, 2.0, 3.0]);
        shared.clear();
        assert_eq!(consumer.len(), 0);
    }

    #[test]
    fn skipping_leaves_the_newest_behind() {
        let shared = ring(8);
        let mut producer = shared.producer().unwrap();
        let mut consumer = shared.consumer().unwrap();

        producer.push(&[1.0, 2.0, 3.0, 4.0]);
        assert_eq!(consumer.skip(2), 2);
        let mut out = [0.0; 2];
        assert_eq!(consumer.pop(&mut out), 2);
        assert_eq!(out, [3.0, 4.0]);
        assert_eq!(consumer.skip(9), 0);
    }

    #[test]
    fn the_two_ends_survive_being_on_different_threads() {
        let shared = ring(64);
        let mut producer = shared.producer().unwrap();
        let mut consumer = shared.consumer().unwrap();

        let writer = std::thread::spawn(move || {
            for i in 0..10_000u32 {
                while producer.push(&[i as f32]) == 0 {
                    std::thread::yield_now();
                }
            }
        });

        let mut expected = 0u32;
        let mut out = [0.0; 1];
        while expected < 10_000 {
            if consumer.pop(&mut out) == 1 {
                assert_eq!(out[0], expected as f32);
                expected += 1;
            }
        }
        writer.join().unwrap();
    }
}
