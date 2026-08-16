//! Sample rate conversion between the three clocks the bridge straddles.
//!
//! Every consumer in the bridge reads from a ring that some other clock fills,
//! so as well as converting rates this tracks how full that ring is and nudges
//! the conversion ratio to hold it steady. Without that the fastest clock
//! eventually overruns the ring and the slowest starves it, and the drift shows
//! up as an echo canceller that stops cancelling.
//!
//! `rubato` was the obvious candidate and does not fit: its resamplers work in
//! fixed-size chunks of planar buffers, while a HAL IO proc is handed a frame
//! count it must fill exactly, from an interleaved ring, without allocating. A
//! windowed-sinc polyphase table is a hundred lines and does fit.

use crate::ring::Consumer;

const TAPS: usize = 32;
const PHASES: usize = 128;

/// How far the ratio may be bent to chase the target fill. Clocks in the same
/// room differ by well under 100 ppm, so 2000 ppm is all headroom, and it is
/// still only three cents of pitch.
const MAX_CORRECTION: f64 = 0.002;
const CORRECTION_GAIN: f64 = 0.005;
/// The measured fill jumps by a whole block every callback, so the loop runs
/// off a one second average of it rather than the raw number.
const FILL_SMOOTHING_SECS: f64 = 1.0;
/// A ring this far above its target is not drifting, it has been handed a
/// burst, and no amount of bending the ratio will get that back. The backlog
/// is dropped in one go instead, which costs a click and saves the latency.
const RESYNC_AT: f64 = 2.0;

/// What a resampler could not deliver, in frames of its input.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Loss {
    /// Input that never arrived, so the output has silence in it.
    pub starved: usize,
    /// Input thrown away to get a ring back down to its target. A burst, not a
    /// glitch: it costs a click and saves the latency it would have added.
    pub dropped: usize,
}

pub struct Resampler {
    kernel: Box<[f32]>,
    channels: usize,
    nominal_step: f64,
    step: f64,
    out_rate: f64,
    target_fill: f64,
    average_fill: f64,
    /// Interleaved input, the first `TAPS - 1` frames carried over from the
    /// previous call so the filter keeps its history.
    staging: Vec<f32>,
    filled: usize,
    position: f64,
    /// Output stays silent until the ring has reached its target once, so a
    /// cold start fills the buffer instead of underrunning against it forever.
    primed: bool,
    starved: usize,
}

impl Resampler {
    /// `target_fill_frames` is the ring level the drift loop aims to hold, and
    /// `max_out_frames` the largest block this will ever be asked for.
    pub fn new(
        in_rate: f64,
        out_rate: f64,
        channels: usize,
        target_fill_frames: usize,
        max_out_frames: usize,
    ) -> Self {
        let nominal_step = in_rate / out_rate;
        let room = TAPS + (nominal_step * 1.1 * max_out_frames as f64).ceil() as usize + 2;
        Self {
            kernel: kernel(nominal_step),
            channels,
            nominal_step,
            step: nominal_step,
            out_rate,
            target_fill: target_fill_frames.max(1) as f64,
            average_fill: target_fill_frames as f64,
            staging: vec![0.0; room * channels],
            filled: TAPS - 1,
            position: (TAPS / 2 - 1) as f64,
            primed: false,
            starved: 0,
        }
    }

    /// Fills `out` with interleaved frames, pulling from `src` as the ratio
    /// demands. Whatever it reports is audio that did not make it, and on a
    /// bridge that is keeping up it reports nothing.
    pub fn process(&mut self, src: &mut Consumer, out: &mut [f32]) -> Loss {
        let out_frames = out.len() / self.channels;
        let mut dropped = 0;

        let mut fill = src.len() / self.channels;
        if fill as f64 > self.target_fill * RESYNC_AT {
            let excess = fill - self.target_fill as usize;
            dropped = src.skip(excess * self.channels) / self.channels;
            fill = src.len() / self.channels;
            self.average_fill = fill as f64;
        }
        self.adapt(fill, out_frames);

        if !self.primed {
            out.fill(0.0);
            return Loss {
                starved: 0,
                dropped,
            };
        }

        let last = self.position + self.step * (out_frames as f64 - 1.0);
        let needed = last.floor() as usize + TAPS / 2 + 1;
        if needed * self.channels > self.staging.len() {
            // Asked for a block bigger than this was built for. Growing the
            // buffer here would allocate on an audio thread, so the block is
            // dropped instead and the caller counts it as starved.
            out.fill(0.0);
            self.reset();
            return Loss {
                starved: out_frames,
                dropped,
            };
        }

        if needed > self.filled {
            let want = (needed - self.filled) * self.channels;
            let got = src.pop(&mut self.staging[self.filled * self.channels..][..want]);
            if got < want {
                self.staging[self.filled * self.channels + got..needed * self.channels].fill(0.0);
                self.starved += (want - got) / self.channels;
                self.primed = false;
            }
            self.filled = needed;
        }

        for frame in 0..out_frames {
            let index = self.position.floor();
            let phase = (self.position - index) * PHASES as f64;
            let low = (phase.floor() as usize).min(PHASES - 1);
            let blend = (phase - low as f64) as f32;
            let base = (index as usize + 1 - TAPS / 2) * self.channels;

            for channel in 0..self.channels {
                let mut sum = 0.0;
                for tap in 0..TAPS {
                    let weight = self.kernel[low * TAPS + tap]
                        + blend
                            * (self.kernel[(low + 1) * TAPS + tap] - self.kernel[low * TAPS + tap]);
                    sum += weight * self.staging[base + tap * self.channels + channel];
                }
                out[frame * self.channels + channel] = sum;
            }
            self.position += self.step;
        }

        let consumed = self.position.floor() as usize + 1 - TAPS / 2;
        self.staging
            .copy_within(consumed * self.channels..self.filled * self.channels, 0);
        self.filled -= consumed;
        self.position -= consumed as f64;

        Loss {
            starved: std::mem::take(&mut self.starved),
            dropped,
        }
    }

    pub fn ratio(&self) -> f64 {
        self.step / self.nominal_step
    }

    fn adapt(&mut self, fill_frames: usize, out_frames: usize) {
        let alpha = (out_frames as f64 / self.out_rate / FILL_SMOOTHING_SECS).min(1.0);
        self.average_fill += (fill_frames as f64 - self.average_fill) * alpha;

        if !self.primed {
            if (fill_frames as f64) < self.target_fill {
                return;
            }
            self.primed = true;
            self.average_fill = fill_frames as f64;
        }

        let error = (self.average_fill - self.target_fill) / self.target_fill;
        let correction = (CORRECTION_GAIN * error).clamp(-MAX_CORRECTION, MAX_CORRECTION);
        self.step = self.nominal_step * (1.0 + correction);
    }

    fn reset(&mut self) {
        self.staging.fill(0.0);
        self.filled = TAPS - 1;
        self.position = (TAPS / 2 - 1) as f64;
        self.primed = false;
    }
}

/// A windowed-sinc filter for every phase, plus one extra row so the phase
/// itself can be interpolated. Downsampling moves the cutoff below the output
/// Nyquist, which is the whole reason this is not a plain interpolator.
fn kernel(step: f64) -> Box<[f32]> {
    let cutoff = 0.92 / step.max(1.0);
    let mut table = vec![0.0f32; (PHASES + 1) * TAPS];

    for phase in 0..=PHASES {
        let frac = phase as f64 / PHASES as f64;
        let row = &mut table[phase * TAPS..][..TAPS];
        for (tap, weight) in row.iter_mut().enumerate() {
            let distance = (TAPS / 2 - 1) as f64 + frac - tap as f64;
            let window_pos = (distance + (TAPS / 2) as f64) / TAPS as f64;
            let window = 0.42 - 0.5 * (2.0 * std::f64::consts::PI * window_pos).cos()
                + 0.08 * (4.0 * std::f64::consts::PI * window_pos).cos();
            *weight = (cutoff * sinc(cutoff * distance) * window) as f32;
        }
        let sum: f32 = row.iter().sum();
        if sum.abs() > f32::EPSILON {
            for weight in row.iter_mut() {
                *weight /= sum;
            }
        }
    }
    table.into_boxed_slice()
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-9 {
        return 1.0;
    }
    let pi_x = std::f64::consts::PI * x;
    pi_x.sin() / pi_x
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ring::{Producer, ring};
    use std::sync::Arc;

    fn feed(rate: f64, freq: f64, frames: usize, channels: usize, offset: usize) -> Vec<f32> {
        (0..frames * channels)
            .map(|i| {
                let frame = (offset + i / channels) as f64;
                (2.0 * std::f64::consts::PI * freq * frame / rate).sin() as f32
            })
            .collect()
    }

    fn rig(
        in_rate: f64,
        out_rate: f64,
        channels: usize,
        target: usize,
        block: usize,
    ) -> (Resampler, Arc<crate::ring::Shared>, Producer) {
        let shared = ring(in_rate as usize * channels);
        let producer = shared.producer().unwrap();
        (
            Resampler::new(in_rate, out_rate, channels, target, block),
            shared,
            producer,
        )
    }

    // Peak of the output against the peak of a pure tone that went in: the
    // point of a windowed sinc over a plain interpolator is that this holds.
    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0f32, |a, b| a.max(b.abs()))
    }

    #[test]
    fn a_tone_survives_forty_eight_to_forty_four_one() {
        let (mut resampler, shared, mut producer) = rig(48_000.0, 44_100.0, 1, 480, 512);
        let mut consumer = shared.consumer().unwrap();
        let mut out = vec![0.0; 512];
        let mut tail = Vec::new();

        for round in 0..40 {
            producer.push(&feed(48_000.0, 1_000.0, 558, 1, round * 558));
            resampler.process(&mut consumer, &mut out);
            if round > 20 {
                tail.extend_from_slice(&out);
            }
        }
        let level = peak(&tail);
        assert!(level > 0.97 && level < 1.03, "peak was {level}");
    }

    #[test]
    fn it_holds_the_rate_it_was_asked_for() {
        let (mut resampler, shared, mut producer) = rig(48_000.0, 44_100.0, 1, 480, 512);
        let mut consumer = shared.consumer().unwrap();
        let mut out = vec![0.0; 512];

        let mut pushed = 0usize;
        for _ in 0..200 {
            producer.push(&vec![0.5; 558]);
            pushed += 558;
            resampler.process(&mut consumer, &mut out);
        }
        // 200 output blocks of 512 frames at the 48k side is 512 * 200 * 48/44.1.
        let consumed = pushed - consumer.len();
        let expected = (512.0 * 200.0 * 48_000.0 / 44_100.0) as usize;
        let drift = consumed.abs_diff(expected) as f64 / expected as f64;
        assert!(
            drift < 0.01,
            "consumed {consumed}, expected about {expected}"
        );
    }

    #[test]
    fn channels_stay_where_they_were_put() {
        let (mut resampler, shared, mut producer) = rig(48_000.0, 48_000.0, 2, 480, 512);
        let mut consumer = shared.consumer().unwrap();
        let mut out = vec![0.0; 1024];

        for _ in 0..20 {
            let mut block = Vec::new();
            for _ in 0..512 {
                block.extend_from_slice(&[1.0, -1.0]);
            }
            producer.push(&block);
            resampler.process(&mut consumer, &mut out);
        }
        // The last block is well past the filter's warm-up.
        for frame in out.chunks(2).skip(64) {
            assert!(frame[0] > 0.9, "left was {}", frame[0]);
            assert!(frame[1] < -0.9, "right was {}", frame[1]);
        }
    }

    #[test]
    fn nothing_comes_out_until_the_ring_has_filled_once() {
        let (mut resampler, shared, mut producer) = rig(48_000.0, 48_000.0, 1, 480, 512);
        let mut consumer = shared.consumer().unwrap();
        let mut out = vec![9.0; 512];

        producer.push(&vec![1.0; 100]);
        resampler.process(&mut consumer, &mut out);
        assert!(out.iter().all(|s| *s == 0.0), "primed too early");

        producer.push(&vec![1.0; 900]);
        resampler.process(&mut consumer, &mut out);
        assert!(out.iter().any(|s| *s != 0.0), "never primed");
    }

    #[test]
    fn a_faster_producer_is_pulled_back_to_the_target() {
        // 200 ppm fast, which is about as far as two real clocks ever get.
        let (mut resampler, shared, mut producer) = rig(48_000.0, 48_000.0, 1, 480, 480);
        let mut consumer = shared.consumer().unwrap();
        let mut out = vec![0.0; 480];

        let mut owed = 0.0f64;
        for _ in 0..6_000 {
            owed += 480.0 * 1.000_2;
            let push = owed.floor() as usize;
            owed -= push as f64;
            producer.push(&vec![0.25; push]);
            resampler.process(&mut consumer, &mut out);
        }
        assert_eq!(shared.lost(), 0, "the ring overran");
        assert!(
            resampler.ratio() > 1.000_1,
            "the loop never sped up: {}",
            resampler.ratio()
        );
        let fill = consumer.len();
        assert!(fill < 480 * 3, "the ring ran away to {fill}");
    }

    #[test]
    fn a_burst_is_dropped_rather_than_drained() {
        let (mut resampler, shared, mut producer) = rig(48_000.0, 48_000.0, 1, 480, 480);
        let mut consumer = shared.consumer().unwrap();
        let mut out = vec![0.0; 480];

        // A second arriving at once, which is what a virtual device does when
        // it starts and catches up with the host clock.
        let pushed = producer.push(&vec![0.5; 48_000]);
        let loss = resampler.process(&mut consumer, &mut out);
        assert_eq!(loss.starved, 0);
        assert!(
            loss.dropped > pushed - 480 * 3,
            "only dropped {} of {pushed}",
            loss.dropped
        );
        assert!(consumer.len() < 480 * 2, "still holding {}", consumer.len());
        assert!(out.iter().any(|s| *s != 0.0), "went silent after the burst");
    }

    #[test]
    fn a_starved_ring_is_reported_and_recovers() {
        let (mut resampler, shared, mut producer) = rig(48_000.0, 48_000.0, 1, 480, 480);
        let mut consumer = shared.consumer().unwrap();
        let mut out = vec![0.0; 480];

        producer.push(&vec![0.5; 480]);
        assert_eq!(resampler.process(&mut consumer, &mut out), Loss::default());
        let loss = resampler.process(&mut consumer, &mut out);
        assert!(loss.starved > 0, "a dry ring was not reported");

        producer.push(&vec![0.5; 960]);
        assert_eq!(resampler.process(&mut consumer, &mut out), Loss::default());
        producer.push(&vec![0.5; 480]);
        assert_eq!(resampler.process(&mut consumer, &mut out), Loss::default());
    }
}
