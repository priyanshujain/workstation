//! RNNoise, wrapped so the bridge can only use it wrong by not using it.
//!
//! The model has three hard requirements and every one of them is a silent
//! failure if it is missed, so none of them are left to the caller:
//!
//! - Samples arrive in i16 units, `[-32768, 32767]`, not the `[-1, 1]` the rest
//!   of the bridge works in. Feeding it `[-1, 1]` does not fail, it just barely
//!   suppresses anything, which is the worst way for this to break.
//! - Frames are exactly 480 samples of mono 48 kHz audio. The bridge's frame is
//!   10 ms of whatever the tap is running at, which is 480 only by coincidence,
//!   so anything else is resampled through 48 kHz and back.
//! - The first frame out of the model is fade-in artefacts. It is replaced with
//!   silence rather than passed on.
//!
//! This has to sit after the echo canceller, never before it, for two reasons.
//! RNNoise is a nonlinear time-varying gain and AEC3 can only subtract an echo
//! it can model linearly, so in front it would leave the canceller chasing a
//! moving target. It also wants the high-passed stream aec3 hands over: the
//! model leans on a pitch estimate, rumble below 80 Hz is enough to spoil that
//! estimate, and the same noisy speech measured 61.7 dB of suppression through
//! the high-pass against 41.0 dB without it.
//!
//! The model's noise estimate is not instant either: on stationary noise it
//! climbs from about 8 dB of suppression after a second to 50 dB after ten,
//! and nothing here can hurry it. Worth knowing before anyone measures a two
//! second clip and concludes it is broken.

use nnnoiseless::DenoiseState;

const FRAME: usize = DenoiseState::FRAME_SIZE;
const MODEL_RATE: f64 = 48_000.0;

/// i16 full scale. A power of two, so the trip out and back is exact and a
/// bypassed denoiser returns the samples it was given, bit for bit.
const SCALE: f32 = 32_768.0;

/// How long the gate takes to open, and to close once the hold has run out.
/// Opening fast enough not to swallow a consonant matters more than closing
/// neatly, and a close that is too quick is what makes a gate breathe.
const ATTACK_MS: f64 = 4.0;
const RELEASE_MS: f64 = 40.0;
/// How long the gate stays open after the last frame of voice. Long enough to
/// carry the gaps inside a sentence, short enough that it is shut again before
/// the next lorry goes past.
const HOLD_MS: f64 = 250.0;

pub struct Denoiser {
    state: Box<DenoiseState<'static>>,
    /// Present only when the bridge is not already running at 48 kHz.
    up: Option<Rate>,
    down: Option<Rate>,
    /// Audio at the model's rate, waiting for a whole frame of it.
    pending: Fifo,
    /// Audio back at the bridge's rate, waiting to be handed out.
    ready: Fifo,
    input: Box<[f32; FRAME]>,
    output: Box<[f32; FRAME]>,
    /// The frame the gate is holding back so it can see one frame past the
    /// audio it is deciding about. Only used while the gate is armed.
    held: Box<[f32; FRAME]>,
    holding: bool,
    chunk: usize,
    enabled: bool,
    started: bool,
    voice: f32,
    previous_voice: f32,
    threshold: f32,
    gain: f32,
    hold: usize,
    attack: f32,
    release: f32,
    hold_frames: usize,
}

impl Denoiser {
    /// `rate` is the rate of the audio that will be handed to [`process`], and
    /// it is the only thing this needs to know: any frame size works, and the
    /// frames do not have to be the same size as each other.
    ///
    /// [`process`]: Denoiser::process
    pub fn new(rate: f64) -> Self {
        let rate = if rate > 0.0 { rate } else { MODEL_RATE };
        // One model frame's worth of the caller's audio. Input is taken this
        // much at a time, which is what keeps every buffer here bounded however
        // large a frame the caller hands over.
        let chunk = (FRAME as f64 * rate / MODEL_RATE).ceil() as usize;

        let (up, down) = if rate == MODEL_RATE {
            (None, None)
        } else {
            tracing::info!(rate, "the tap is not at 48 kHz, resampling for the model");
            (
                Some(Rate::new(rate, MODEL_RATE, chunk)),
                Some(Rate::new(MODEL_RATE, rate, FRAME)),
            )
        };

        // The most either queue can be holding: a frame that has not been
        // filled yet, plus everything one chunk can turn into.
        let per_chunk = (chunk as f64 * MODEL_RATE / rate).ceil() as usize;
        let per_ms = MODEL_RATE as f32 / 1000.0;
        let mut denoiser = Denoiser {
            state: DenoiseState::new(),
            up,
            down,
            pending: Fifo::new(FRAME + per_chunk + TAPS),
            ready: Fifo::new(6 * (chunk + TAPS)),
            input: Box::new([0.0; FRAME]),
            output: Box::new([0.0; FRAME]),
            held: Box::new([0.0; FRAME]),
            holding: false,
            chunk,
            enabled: false,
            started: false,
            voice: 0.0,
            previous_voice: 0.0,
            threshold: 0.0,
            gain: 1.0,
            hold: 0,
            attack: 1.0 / (ATTACK_MS as f32 * per_ms),
            release: 1.0 / (RELEASE_MS as f32 * per_ms),
            hold_frames: (HOLD_MS / 10.0) as usize,
        };
        denoiser.reset();
        denoiser
    }

    /// Suppresses noise in `samples`, in place. Switched off it returns without
    /// touching them, so the bridge pays nothing for a denoiser nobody wants.
    pub fn process(&mut self, samples: &mut [f32]) {
        if !self.enabled {
            return;
        }
        for chunk in samples.chunks_mut(self.chunk) {
            self.run(chunk);
        }
    }

    /// Turning it back on starts from silence rather than from whatever was
    /// left in the buffers when it went off, which would otherwise be replayed
    /// several frames late.
    pub fn set_enabled(&mut self, enabled: bool) {
        if enabled != self.enabled {
            self.enabled = enabled;
            self.reset();
        }
    }

    /// Below this voice probability the gate closes. Zero leaves it open.
    pub fn set_threshold(&mut self, threshold: f32) {
        self.threshold = if threshold.is_nan() {
            0.0
        } else {
            threshold.clamp(0.0, 1.0)
        };
    }

    /// The model's opinion of how likely the last frame was to be speech, or
    /// zero when it is not running.
    pub fn voice(&self) -> f32 {
        if self.enabled { self.voice } else { 0.0 }
    }

    fn reset(&mut self) {
        self.state = DenoiseState::new();
        self.pending.clear();
        // The queue starts a frame ahead of itself. Anything held back waiting
        // for a whole frame is then already covered by what is waiting to go
        // out, which is what stops a caller whose frames do not divide into
        // the model's from being handed a hole every few blocks. It costs a
        // frame of delay, and it is the only delay the suppressor adds.
        self.ready.silence(self.chunk + 2 * TAPS);
        self.started = false;
        self.voice = 0.0;
        self.previous_voice = 0.0;
        self.holding = false;
        self.gain = 1.0;
        self.hold = 0;
        if let (Some(up), Some(down)) = (&mut self.up, &mut self.down) {
            up.reset();
            down.reset();
        }
    }

    /// One chunk in, the same number of samples out, a frame later.
    fn run(&mut self, chunk: &mut [f32]) {
        match &mut self.up {
            Some(up) => up.process(chunk, &mut self.pending),
            None => self.pending.push(chunk),
        }

        while self.pending.len() >= FRAME {
            self.pending.pop(&mut self.input[..]);
            for sample in self.input.iter_mut() {
                *sample *= SCALE;
            }
            self.previous_voice = self.voice;
            self.voice = self
                .state
                .process_frame(&mut self.output[..], &self.input[..]);

            if self.started {
                for sample in self.output.iter_mut() {
                    *sample /= SCALE;
                }
            } else {
                // The model fades in over its first frame, so that frame is
                // never worth hearing.
                self.started = true;
                self.output.fill(0.0);
            }

            if self.threshold > 0.0 {
                if !self.holding {
                    // Arming costs a frame of silence, once.
                    self.holding = true;
                    self.held.fill(0.0);
                }
                std::mem::swap(&mut self.held, &mut self.output);
                self.gate();
            } else if self.holding {
                // Disarming hands back the frame the gate was sitting on
                // rather than dropping it and leaving a hole.
                self.holding = false;
                self.gain = 1.0;
                emit(&mut self.down, &mut self.ready, &self.held[..]);
            }
            emit(&mut self.down, &mut self.ready, &self.output[..]);
        }

        // The queue is primed to cover any chunk, so this is only ever a
        // backstop against a caller asking for more than a whole frame more
        // than it gave.
        let short = chunk.len().saturating_sub(self.ready.len());
        chunk[..short].fill(0.0);
        self.ready.pop(&mut chunk[short..]);
    }

    /// Rides a gain over the frame rather than cutting it, and decides on the
    /// frame after the one it is working on as well as the one it is: the
    /// model's confidence takes a frame or two to climb at the start of a word,
    /// so a gate that only looks at the audio in front of it takes the front
    /// off every sentence. That lookahead is the 10 ms the gate costs, and it
    /// is only paid while the gate is armed.
    fn gate(&mut self) {
        let likely = self.voice.max(self.previous_voice);
        if likely >= self.threshold {
            self.hold = self.hold_frames;
        } else {
            self.hold = self.hold.saturating_sub(1);
        }

        let (target, step) = if self.hold > 0 {
            (1.0, self.attack)
        } else {
            (0.0, -self.release)
        };
        for sample in self.output.iter_mut() {
            self.gain = if step > 0.0 {
                (self.gain + step).min(target)
            } else {
                (self.gain + step).max(target)
            };
            *sample *= self.gain;
        }
    }
}

/// Free rather than a method so that the frame being written and the queue it
/// is going into can be borrowed out of the same struct.
fn emit(down: &mut Option<Rate>, ready: &mut Fifo, frame: &[f32]) {
    match down {
        Some(down) => down.process(frame, ready),
        None => ready.push(frame),
    }
}

/// A first in, first out queue over one allocation. What is left after a read
/// is shifted down, which costs a memmove of well under a frame and buys reads
/// and writes that are both plain contiguous slices.
struct Fifo {
    buf: Box<[f32]>,
    len: usize,
}

impl Fifo {
    fn new(capacity: usize) -> Self {
        Fifo {
            buf: vec![0.0; capacity].into_boxed_slice(),
            len: 0,
        }
    }

    fn len(&self) -> usize {
        self.len
    }

    fn push(&mut self, src: &[f32]) {
        // Every capacity here is worked out to fit the worst case, so a full
        // queue is a bug. Dropping the overflow is still better than panicking
        // in something that is feeding a call.
        debug_assert!(src.len() <= self.buf.len() - self.len);
        let take = src.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + take].copy_from_slice(&src[..take]);
        self.len += take;
    }

    fn push_one(&mut self, sample: f32) {
        debug_assert!(self.len < self.buf.len());
        if self.len < self.buf.len() {
            self.buf[self.len] = sample;
            self.len += 1;
        }
    }

    fn pop(&mut self, dst: &mut [f32]) {
        let take = dst.len().min(self.len);
        dst[..take].copy_from_slice(&self.buf[..take]);
        self.buf.copy_within(take..self.len, 0);
        self.len -= take;
    }

    fn clear(&mut self) {
        self.len = 0;
    }

    /// Throws away what is queued and starts again from `samples` of silence.
    fn silence(&mut self, samples: usize) {
        self.len = samples.min(self.buf.len());
        self.buf[..self.len].fill(0.0);
    }
}

const TAPS: usize = 32;
const PHASES: usize = 128;

/// A fixed-ratio polyphase resampler, for the two hops in and out of the
/// model's 48 kHz.
///
/// [`crate::resample::Resampler`] is the wrong tool for this despite being the
/// same filter: it reads from a ring and bends its own ratio to hold that ring
/// level, because it exists to join two clocks that disagree. Both ends of this
/// hop are the same clock and the ratio is exact, so a drift loop here would
/// only add wow to a signal that has none.
struct Rate {
    kernel: Box<[f32]>,
    step: f64,
    /// Input, with the taps either side of the read position carried over from
    /// the last call so the filter keeps its history.
    history: Box<[f32]>,
    filled: usize,
    position: f64,
}

impl Rate {
    /// `max_in` is the largest block this will ever be handed at once.
    fn new(in_rate: f64, out_rate: f64, max_in: usize) -> Self {
        let step = in_rate / out_rate;
        Rate {
            kernel: kernel(step),
            step,
            history: vec![0.0; TAPS + max_in].into_boxed_slice(),
            filled: TAPS - 1,
            position: (TAPS / 2 - 1) as f64,
        }
    }

    fn process(&mut self, input: &[f32], out: &mut Fifo) {
        debug_assert!(input.len() <= self.history.len() - self.filled);
        let take = input.len().min(self.history.len() - self.filled);
        self.history[self.filled..self.filled + take].copy_from_slice(&input[..take]);
        self.filled += take;

        while (self.position as usize) + TAPS / 2 < self.filled {
            out.push_one(self.sample());
            self.position += self.step;
        }

        // Only ever short of `filled` at a ratio far past anything a device can
        // be set to, and clamping keeps that a transient rather than a panic.
        let consumed = (self.position as usize + 1 - TAPS / 2).min(self.filled);
        self.history.copy_within(consumed..self.filled, 0);
        self.filled -= consumed;
        self.position -= consumed as f64;
    }

    fn sample(&self) -> f32 {
        let index = self.position as usize;
        let phase = (self.position - index as f64) * PHASES as f64;
        let low = (phase as usize).min(PHASES - 1);
        let blend = (phase - low as f64) as f32;
        let base = index + 1 - TAPS / 2;

        let mut sum = 0.0;
        for tap in 0..TAPS {
            let near = self.kernel[low * TAPS + tap];
            let far = self.kernel[(low + 1) * TAPS + tap];
            sum += (near + blend * (far - near)) * self.history[base + tap];
        }
        sum
    }

    fn reset(&mut self) {
        self.history.fill(0.0);
        self.filled = TAPS - 1;
        self.position = (TAPS / 2 - 1) as f64;
    }
}

/// A windowed-sinc filter for every phase, plus a row past the end so the phase
/// itself can be interpolated. Downsampling pulls the cutoff below the output
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

    const RATE: f64 = 48_000.0;
    /// Where the speech starts in [`noisy_speech`], in seconds.
    const ONSET: f64 = 9.0;

    /// A vowel: a glottal pulse train through three formants, with the pitch
    /// drifting the way a real one does. Not speech, but it has the harmonic
    /// structure and the spectral shape the model was trained to keep, which
    /// white noise deliberately does not.
    fn speech(samples: usize, rate: f64) -> Vec<f32> {
        let mut out = vec![0.0f32; samples];
        let mut resonators = [[0.0f64; 2]; 3];
        let mut phase = 0.0f64;

        for (i, sample) in out.iter_mut().enumerate() {
            let t = i as f64 / rate;
            phase += (110.0 + 35.0 * (2.0 * std::f64::consts::PI * 0.7 * t).sin()) / rate;
            let excitation = if phase >= 1.0 {
                phase -= 1.0;
                1.0
            } else {
                0.0
            };
            let mut sum = 0.0;
            for (formant, state) in [700.0, 1220.0, 2600.0].iter().zip(&mut resonators) {
                let r = 0.98;
                let theta = 2.0 * std::f64::consts::PI * formant / rate;
                let y = excitation + 2.0 * r * theta.cos() * state[0] - r * r * state[1];
                state[1] = state[0];
                state[0] = y;
                sum += y;
            }
            *sample = (sum * 0.02) as f32;
        }
        out
    }

    fn noise(samples: usize, gain: f32, seed: u32) -> Vec<f32> {
        let mut state = seed;
        (0..samples)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((state >> 8) as f32 / (1 << 23) as f32 - 1.0) * gain
            })
            .collect()
    }

    /// Twelve seconds at `rate`: white noise throughout, speech over the last
    /// three, about 9 dB apart. High-passed, because that is how it arrives:
    /// aec3 runs its high-pass filter before any of this and the model needs
    /// it, so full-band noise would be measuring a case the bridge never has.
    pub fn noisy_speech(rate: f64) -> Vec<f32> {
        let samples = (rate * 12.0) as usize;
        let onset = (rate * ONSET) as usize;
        let mut out = noise(samples, 0.03, 11);
        for (sample, voice) in out[onset..].iter_mut().zip(&speech(samples, rate)[onset..]) {
            *sample += voice;
        }
        high_pass(&out, rate)
    }

    /// Two poles at 80 Hz, near enough to what aec3's high-pass filter does.
    fn high_pass(x: &[f32], rate: f64) -> Vec<f32> {
        let mut y = x.to_vec();
        let a = (-2.0 * std::f64::consts::PI * 80.0 / rate).exp() as f32;
        for _ in 0..2 {
            let (mut previous_in, mut previous_out) = (0.0f32, 0.0f32);
            for sample in y.iter_mut() {
                previous_out = a * (previous_out + *sample - previous_in);
                previous_in = *sample;
                *sample = previous_out;
            }
        }
        y
    }

    pub fn power(samples: &[f32]) -> f64 {
        samples.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / samples.len().max(1) as f64
    }

    /// The noise floor, as the median of the 10 ms frame powers. The mean is
    /// no use for this: the model lets the odd frame through and a handful of
    /// those carry the whole average.
    pub fn floor(samples: &[f32], rate: f64) -> f64 {
        let mut frames: Vec<f64> = samples.chunks(rate as usize / 100).map(power).collect();
        frames.sort_by(|a, b| a.partial_cmp(b).unwrap());
        frames[frames.len() / 2]
    }

    fn db(a: f64, b: f64) -> f64 {
        10.0 * (a / b.max(1e-30)).log10()
    }

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0f32, |a, b| a.max(b.abs()))
    }

    fn denoiser(rate: f64, threshold: f32) -> Denoiser {
        let mut denoiser = Denoiser::new(rate);
        denoiser.set_enabled(true);
        denoiser.set_threshold(threshold);
        denoiser
    }

    fn run(denoiser: &mut Denoiser, input: &[f32], frame: usize) -> Vec<f32> {
        let mut out = input.to_vec();
        for block in out.chunks_mut(frame) {
            denoiser.process(block);
        }
        out
    }

    /// How far the noise floor dropped and how much of the voice went with it,
    /// both in dB. Measured at the end of each stretch: the model's noise
    /// estimate climbs for the first ten seconds or so, from 8 dB of
    /// suppression after one second to 50 dB after eleven, so where this looks
    /// matters as much as what it looks at.
    fn suppression(input: &[f32], output: &[f32], rate: f64) -> (f64, f64) {
        let at = |seconds: f64| (rate * seconds) as usize;
        let quiet = at(ONSET - 2.0)..at(ONSET);
        let loud = at(ONSET + 1.0)..input.len();
        (
            db(
                floor(&input[quiet.clone()], rate),
                floor(&output[quiet], rate),
            ),
            db(power(&input[loud.clone()]), power(&output[loud])),
        )
    }

    #[test]
    fn it_takes_the_noise_out_and_leaves_the_voice() {
        let input = noisy_speech(RATE);
        let output = run(&mut denoiser(RATE, 0.0), &input, 480);
        let (noise_down, voice_down) = suppression(&input, &output, RATE);

        assert!(
            noise_down > 25.0,
            "only took the noise down {noise_down:.1} dB"
        );
        assert!(
            voice_down < 2.0,
            "took {voice_down:.1} dB off the voice too"
        );
        // What the far end actually hears: the two figures together, against
        // the 9 dB it started with.
        assert!(
            noise_down - voice_down > 24.0,
            "signal to noise only improved {:.1} dB",
            noise_down - voice_down
        );
    }

    // The model wants i16 units and says nothing when it does not get them, it
    // simply stops working: the same audio in [-1, 1] barely moves the noise
    // floor at all. This is the whole reason the scaling is not the caller's
    // problem, so it is worth proving rather than commenting.
    #[test]
    fn the_model_is_fed_in_i16_units() {
        let input = noisy_speech(RATE);
        let (scaled, _) = suppression(&input, &run(&mut denoiser(RATE, 0.0), &input, 480), RATE);

        let mut state = DenoiseState::new();
        let mut raw = input.clone();
        for block in raw.chunks_mut(FRAME) {
            let mut out = [0.0f32; FRAME];
            state.process_frame(&mut out, block);
            block.copy_from_slice(&out);
        }
        let (unscaled, _) = suppression(&input, &raw, RATE);

        assert!(unscaled < 3.0, "[-1, 1] suppressed {unscaled:.1} dB");
        assert!(
            scaled > unscaled + 25.0,
            "scaled took the noise down {scaled:.1} dB against {unscaled:.1} dB unscaled"
        );
    }

    // The other half of the scaling: forget the way back and everything is 90 dB
    // too loud.
    #[test]
    fn what_comes_out_is_at_the_level_that_went_in() {
        let input = noisy_speech(RATE);
        let output = run(&mut denoiser(RATE, 0.0), &input, 480);
        let voiced = (RATE * (ONSET + 1.0)) as usize..;
        let (before, after) = (peak(&input[voiced.clone()]), peak(&output[voiced]));
        assert!(
            after > before * 0.25 && after < before * 2.0,
            "went in at {before:.3} and came out at {after:.3}"
        );
    }

    #[test]
    fn switched_off_it_hands_back_exactly_what_it_was_given() {
        let input = noisy_speech(RATE);
        let mut denoiser = Denoiser::new(RATE);
        assert_eq!(run(&mut denoiser, &input, 480), input);

        // And still, bit for bit, after it has been on and gone off again.
        denoiser.set_enabled(true);
        run(&mut denoiser, &input, 480);
        denoiser.set_enabled(false);
        assert_eq!(run(&mut denoiser, &input, 480), input);
    }

    // The bridge hands over 10 ms of whatever the tap is running at, which is
    // 480 samples only while it is at 48 kHz, and nothing says a caller has to
    // be consistent about it either.
    #[test]
    fn the_frame_size_it_is_handed_does_not_matter() {
        let input = noisy_speech(RATE);
        let straight = run(&mut denoiser(RATE, 0.0), &input, 480);

        for frame in [1, 137, 480, 1024] {
            let odd = run(&mut denoiser(RATE, 0.0), &input, frame);
            assert_eq!(odd.len(), input.len());
            let lag = (0..2 * FRAME)
                .find(|lag| {
                    odd[*lag..]
                        .iter()
                        .zip(&straight)
                        .all(|(a, b)| (a - b).abs() < 1e-9)
                })
                .unwrap_or_else(|| panic!("frames of {frame} gave different audio"));
            assert!(
                lag <= FRAME,
                "frames of {frame} cost {lag} samples of delay"
            );
        }

        // Sizes that change from call to call, which is what a resampled
        // capture stream does when it is a sample short of a frame.
        let mut denoiser = denoiser(RATE, 0.0);
        let mut mixed = input.clone();
        let mut at = 0;
        for size in [479usize, 480, 481, 1, 960, 137].iter().cycle() {
            let end = (at + size).min(mixed.len());
            denoiser.process(&mut mixed[at..end]);
            at = end;
            if at == mixed.len() {
                break;
            }
        }
        let (noise_down, voice_down) = suppression(&input, &mixed, RATE);
        assert!(
            noise_down > 25.0,
            "only took the noise down {noise_down:.1} dB"
        );
        assert!(
            voice_down < 2.0,
            "took {voice_down:.1} dB off the voice too"
        );
    }

    // The tap publishes thirteen sample rates and the model only speaks one of
    // them, so everything else goes through 48 kHz and back.
    #[test]
    fn a_tap_that_is_not_at_forty_eight_is_still_denoised() {
        for rate in [44_100.0, 96_000.0] {
            let input = noisy_speech(rate);
            let output = run(&mut denoiser(rate, 0.0), &input, rate as usize / 100);
            assert_eq!(output.len(), input.len());

            let (noise_down, voice_down) = suppression(&input, &output, rate);
            assert!(
                noise_down > 20.0,
                "at {rate} only took the noise down {noise_down:.1} dB"
            );
            assert!(
                voice_down < 2.0,
                "at {rate} took {voice_down:.1} dB off the voice too"
            );
        }
    }

    #[test]
    fn the_gate_shuts_when_nobody_is_talking() {
        let input = noisy_speech(RATE);
        let output = run(&mut denoiser(RATE, 0.8), &input, 480);
        let quiet = 5 * 48_000..8 * 48_000;
        assert!(
            db(power(&input[quiet.clone()]), power(&output[quiet])) > 60.0,
            "the gate left the noise where it was"
        );
    }

    // A gate that waits for the model to be sure takes the front off every
    // sentence. This one is allowed to lose the first few milliseconds, since
    // the model itself is only half convinced there, but not the word.
    #[test]
    fn the_gate_does_not_take_the_front_off_a_word() {
        let input = noisy_speech(RATE);
        let open = run(&mut denoiser(RATE, 0.0), &input, 480);
        let gated = run(&mut denoiser(RATE, 0.8), &input, 480);

        // An armed gate holds a frame back to see what is coming, so its
        // stream runs 10 ms behind the one that is wide open.
        let kept = |window: std::ops::Range<usize>| {
            power(&gated[window.start + FRAME..window.end + FRAME]) / power(&open[window])
        };
        let onset = (RATE * ONSET) as usize;
        let first = kept(onset..onset + 1_440);
        let rest = kept(onset + 1_440..onset + 48_000);
        assert!(
            first > 0.8,
            "only {:.0}% of the first 30 ms survived",
            first * 100.0
        );
        assert!(
            rest > 0.95,
            "only {:.0}% of the word survived",
            rest * 100.0
        );
    }

    #[test]
    fn turning_it_on_again_does_not_replay_the_old_buffers() {
        let mut denoiser = denoiser(RATE, 0.0);
        run(&mut denoiser, &noisy_speech(RATE), 480);
        denoiser.set_enabled(false);
        denoiser.set_enabled(true);

        let silence = vec![0.0f32; 48_000];
        let out = run(&mut denoiser, &silence, 480);
        assert_eq!(peak(&out), 0.0, "the buffers came back out");
    }

    #[test]
    fn the_threshold_it_is_given_is_the_threshold_it_keeps() {
        let mut denoiser = Denoiser::new(RATE);
        denoiser.set_threshold(2.0);
        assert_eq!(denoiser.threshold, 1.0);
        denoiser.set_threshold(-1.0);
        assert_eq!(denoiser.threshold, 0.0);
        denoiser.set_threshold(f32::NAN);
        assert_eq!(denoiser.threshold, 0.0);
    }

    // The worker thread has 10 ms to turn a frame around and this is the only
    // thing in it that is not a memcpy. It is nowhere near, but the crate plans
    // its FFTs behind a thread local on first use, so the first frame costs
    // more than the rest and that is worth knowing about.
    #[test]
    fn a_frame_costs_a_fraction_of_the_frame_it_fills() {
        let mut denoiser = denoiser(RATE, 0.9);
        let input = noisy_speech(RATE);
        let mut worst = std::time::Duration::ZERO;
        let started = std::time::Instant::now();

        let mut frames = 0;
        let mut block = [0.0f32; 480];
        for chunk in input.chunks(480) {
            block[..chunk.len()].copy_from_slice(chunk);
            let at = std::time::Instant::now();
            denoiser.process(&mut block[..chunk.len()]);
            worst = worst.max(at.elapsed());
            frames += 1;
        }
        let mean = started.elapsed().as_secs_f64() * 1e3 / frames as f64;
        println!(
            "{frames} frames, mean {mean:.3} ms, worst {:.3} ms",
            worst.as_secs_f64() * 1e3
        );
        // A debug build runs the model tens of times slower than the one the
        // bridge is built with, so what counts as comfortable depends on the
        // build. Either bound still catches a frame that has become expensive
        // rather than one that always was.
        let budget = if cfg!(debug_assertions) { 5.0 } else { 1.0 };
        assert!(mean < budget, "a frame took {mean:.3} ms of its 10 ms");
    }
}

#[cfg(test)]
mod against_aec3 {
    use super::tests::{floor, noisy_speech};
    use super::*;
    use crate::aec::Canceller;

    /// Speech in white noise through the canceller, with aec3's own noise
    /// suppressor on or off and RNNoise after it on or off. No echo, so the
    /// only thing moving the noise floor is the suppressors.
    fn noise_down(aec3_ns: bool, rnnoise: bool) -> f64 {
        let input = noisy_speech(48_000.0);
        let mut canceller = Canceller::new(48_000, 48_000, 100).unwrap();
        canceller.set_noise_suppression(aec3_ns).unwrap();
        let mut denoiser = Denoiser::new(48_000.0);
        denoiser.set_enabled(rnnoise);

        let silence = [0.0f32; FRAME];
        let mut output = vec![0.0f32; input.len()];
        let mut cleaned = vec![0.0f32; FRAME];
        for (i, frame) in input.chunks(FRAME).enumerate() {
            canceller.render(&silence).unwrap();
            if canceller.capture(frame, &mut cleaned).unwrap() {
                denoiser.process(&mut cleaned);
                output[i * FRAME..][..FRAME].copy_from_slice(&cleaned);
            }
        }
        let quiet = 7 * 48_000..9 * 48_000;
        10.0 * (floor(&input[quiet.clone()], 48_000.0) / floor(&output[quiet], 48_000.0).max(1e-30))
            .log10()
    }

    // Why aec3 hands its noise suppressor over rather than working alongside
    // RNNoise. Two in series are worse than the better one alone, because what
    // aec3 leaves behind is no longer the stationary noise RNNoise was trained
    // to find, and this is the test that says so out loud.
    #[test]
    fn two_suppressors_are_worse_than_the_better_one_alone() {
        let (aec3, rnnoise, both) = (
            noise_down(true, false),
            noise_down(false, true),
            noise_down(true, true),
        );
        println!("aec3 {aec3:.1} dB, rnnoise {rnnoise:.1} dB, both {both:.1} dB");
        assert!(
            rnnoise > aec3 + 15.0,
            "aec3 {aec3:.1} dB, rnnoise {rnnoise:.1} dB"
        );
        assert!(
            rnnoise > both + 10.0,
            "both {both:.1} dB, rnnoise alone {rnnoise:.1} dB"
        );
    }
}
