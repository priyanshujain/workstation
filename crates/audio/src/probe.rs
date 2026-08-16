//! The probe the calibrator plays, and the maths that turns a recording of it
//! back into a delay.
//!
//! The probe is an exponential sine sweep, recovered with Farina's inverse
//! filter rather than by correlating against the sweep itself. Three properties
//! decide it over a click, a noise burst or a maximum-length sequence:
//!
//! - It puts a lot of energy into the room for a modest peak level. A click
//!   loud enough to hear over a fan has to be near full scale; a sweep spread
//!   over 400 ms does not, which matters when the speaker is six inches from
//!   the microphone.
//! - Deconvolution is a matched filter, so 400 ms of sweep collapses into one
//!   sample and everything uncorrelated with it, which is the entire rest of
//!   the room, does not. Traffic outside and a laptop fan cost peak height,
//!   not peak position.
//! - A small speaker driven at any useful level distorts. An exponential sweep
//!   puts every harmonic of that distortion *before* the linear arrival in the
//!   deconvolved response, so it can be stepped over rather than mistaken for
//!   an early one. A maximum-length sequence smears the same distortion across
//!   the whole response, which is exactly where the peak is being looked for.
//!
//! Nothing here touches a device. It is fed samples and hands back a lag, so
//! the whole of it can be tested against signals with a known delay in them.

use std::f64::consts::PI;

/// The band the sweep covers. A laptop speaker has nothing useful below a
/// couple of hundred hertz, and the top is kept below the point where a
/// Bluetooth codec starts throwing detail away.
const LOW_HZ: f64 = 200.0;
const HIGH_HZ: f64 = 10_000.0;

/// Peak level of the sweep. Deliberately quiet: the room has to be excited,
/// not filled, and the calibrator is often run with the microphone a hand's
/// width from the speaker.
const AMPLITUDE: f64 = 0.2;

/// Raised cosine in and out, so the sweep neither clicks nor asks the speaker
/// for a step it cannot make.
const FADE_MS: f64 = 15.0;

/// How far either side of the peak counts as part of the same arrival rather
/// than as background.
///
/// This started at 2 ms and that was wrong, in a way worth writing down. A
/// loudspeaker is a resonant object: measured against a real one the response
/// peaked at 51.2 ms and had a second point at 55.2 ms only 1.3 times smaller,
/// with the whole arrival smeared from 38 to 67 ms. Every one of those is the
/// same sound still ringing. A narrow guard compares the arrival against itself
/// and reports a perfectly good measurement as ambiguous, which is how a room
/// full of working hardware measured nothing at all.
const GUARD_MS: f64 = 25.0;

/// How much of the peak an earlier point has to reach to be taken as the real
/// start of the arrival.
///
/// The tallest point in the response is not always the direct sound. Measured
/// across six probes at one fixed speaker and microphone, two points 4 ms apart
/// came back within 2% of each other and which of them was taller changed from
/// probe to probe, so reading the delay off the tallest made the answer wobble
/// by 4 ms between probes that were otherwise identical. The first arrival is
/// both the steadier reading and the physically right one, since the direct
/// path is the short one and everything after it is the room. It is also the
/// safer one to hand aec3, whose filter spans forward from the delay it is
/// given and can absorb a reflection it was told about early.
const ONSET: f64 = 0.3;

/// How far the peak has to stand above the background before the measurement is
/// believed. The gap it sits in is wide: a recording with no probe in it at all
/// gives 4 to 5, since the tallest of twenty-odd thousand noise samples is
/// always a few times their RMS, while a real arrival off a laptop speaker gives
/// several hundred.
const CLEAR: f64 = 12.0;

/// Whether a peak that tall is worth reporting. A false here is a failed
/// measurement, not a delay of wherever the noise happened to be tallest.
pub fn is_clear(confidence: f64) -> bool {
    confidence >= CLEAR
}

/// A sweep and the filter that recovers it, both at one sample rate.
pub struct Probe {
    rate: f64,
    signal: Vec<f32>,
    filter: Vec<f64>,
}

/// Where the probe was found in a recording, and how sure that is.
#[derive(Clone, Copy, Debug)]
pub struct Arrival {
    pub delay_samples: f64,
    /// Peak height over the background: everything in the searched response
    /// more than [`GUARD_MS`] away from the peak, taken as an RMS rather than a
    /// single tallest rival so that one spike of speaker distortion cannot
    /// condemn an otherwise clean measurement.
    pub confidence: f64,
}

impl Arrival {
    pub fn delay_ms(&self, rate: f64) -> f64 {
        self.delay_samples * 1000.0 / rate
    }
}

impl Probe {
    pub fn new(rate: f64, seconds: f64) -> Self {
        let signal = sweep(rate, seconds);
        let filter = inverse(&signal, rate);
        Probe {
            rate,
            signal,
            filter,
        }
    }

    /// The samples to play, mono, at the rate this was built for.
    pub fn signal(&self) -> &[f32] {
        &self.signal
    }

    /// Samples the recording has to hold beyond the moment the probe started
    /// for a lag of up to `max_lag` to be findable.
    pub fn needs(&self, max_lag: usize) -> usize {
        self.signal.len() + max_lag
    }

    /// Finds the probe in `recording`, which must begin at the instant the
    /// probe's first sample went to the speaker. `None` means the recording is
    /// too short or holds nothing at all; a lag is only ever reported together
    /// with how clear it was.
    ///
    /// The two need not have been sampled by the same device, only at the same
    /// nominal rate: the sweep is a function of time, so generating it once per
    /// endpoint is the same signal twice rather than a resampling problem.
    pub fn find(&self, recording: &[f32], max_lag: usize) -> Option<Arrival> {
        let n = self.filter.len();
        let wanted = self.needs(max_lag);
        if recording.len() < wanted {
            return None;
        }
        let response = convolve(&recording[..wanted], &self.filter);

        // Deconvolution lands a lag of zero at the last sample of the filter,
        // so everything before that is the distortion the speaker added and is
        // not searched.
        let window = &response[n - 1..=n - 1 + max_lag];
        peak(window, (self.rate * GUARD_MS / 1000.0) as usize)
    }
}

/// An exponential sweep from [`LOW_HZ`] to [`HIGH_HZ`]: instantaneous frequency
/// `LOW_HZ * e^(t/l)`, so every octave gets the same time and the same energy.
fn sweep(rate: f64, seconds: f64) -> Vec<f32> {
    let count = (rate * seconds) as usize;
    let l = (count as f64 / rate) / (HIGH_HZ / LOW_HZ).ln();
    let k = 2.0 * PI * LOW_HZ * l;
    let fade = ((rate * FADE_MS / 1000.0) as usize).min(count / 2).max(1);

    (0..count)
        .map(|i| {
            let phase = k * ((i as f64 / rate / l).exp() - 1.0);
            (phase.sin() * AMPLITUDE * taper(i, count, fade)) as f32
        })
        .collect()
}

fn taper(i: usize, count: usize, fade: usize) -> f64 {
    let rise = |x: usize| 0.5 - 0.5 * (PI * x as f64 / fade as f64).cos();
    if i < fade {
        rise(i)
    } else if i + fade >= count {
        rise(count - 1 - i)
    } else {
        1.0
    }
}

/// Farina's inverse filter: the sweep backwards, with the low end pulled down
/// by the same 6 dB per octave the sweep's own spectrum leans on. Without that
/// tilt the deconvolution gives a pink smear instead of a spike.
fn inverse(sweep: &[f32], rate: f64) -> Vec<f64> {
    let count = sweep.len();
    let l = (count as f64 / rate) / (HIGH_HZ / LOW_HZ).ln();
    (0..count)
        .map(|i| sweep[count - 1 - i] as f64 * (-(i as f64) / (l * rate)).exp())
        .collect()
}

/// The tallest point of the response and how far it stands above the background
/// more than `guard` away from it.
fn peak(window: &[f64], guard: usize) -> Option<Arrival> {
    let (at, height) = window
        .iter()
        .enumerate()
        .map(|(i, v)| (i, v.abs()))
        .max_by(|a, b| a.1.total_cmp(&b.1))?;
    if height <= 0.0 {
        return None;
    }

    let (sum, count) = window
        .iter()
        .enumerate()
        .filter(|(i, _)| i.abs_diff(at) > guard)
        .fold((0.0, 0usize), |(sum, count), (_, v)| {
            (sum + v * v, count + 1)
        });
    if count == 0 {
        return None;
    }
    let background = (sum / count as f64).sqrt();

    // Back to the front of the arrival, which is the direct sound. Bounded by
    // the guard, so this can only ever walk within one arrival and never back
    // into the speaker's distortion or the previous probe.
    let onset = window[at.saturating_sub(guard)..=at]
        .iter()
        .position(|v| v.abs() >= height * ONSET)
        .map_or(at, |i| at.saturating_sub(guard) + i);

    Some(Arrival {
        delay_samples: onset as f64,
        // A synthetic response with no background at all would divide by zero,
        // and an unbounded confidence is no more useful than a large one.
        confidence: height / background.max(height * 1e-6),
    })
}

/// Linear convolution through the frequency domain. The direct form would be
/// tens of billions of multiply-adds for a run of this length.
fn convolve(a: &[f32], b: &[f64]) -> Vec<f64> {
    let len = a.len() + b.len() - 1;
    let size = len.next_power_of_two();

    let mut ar = vec![0.0; size];
    let mut ai = vec![0.0; size];
    let mut br = vec![0.0; size];
    let mut bi = vec![0.0; size];
    for (dst, src) in ar.iter_mut().zip(a) {
        *dst = *src as f64;
    }
    br[..b.len()].copy_from_slice(b);

    fft(&mut ar, &mut ai, false);
    fft(&mut br, &mut bi, false);
    for i in 0..size {
        let re = ar[i] * br[i] - ai[i] * bi[i];
        let im = ar[i] * bi[i] + ai[i] * br[i];
        ar[i] = re;
        ai[i] = im;
    }
    fft(&mut ar, &mut ai, true);

    ar.truncate(len);
    ar
}

/// In-place radix-2 Cooley-Tukey. Twiddles are computed rather than stepped
/// round the unit circle: a run of this length would accumulate enough error in
/// the recurrence to blunt the peak, and a few million sine calls cost less
/// than being wrong.
fn fft(re: &mut [f64], im: &mut [f64], inverse: bool) {
    let n = re.len();
    debug_assert!(n.is_power_of_two() && im.len() == n);

    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }

    let mut span = 2;
    while span <= n {
        let step = if inverse { 2.0 * PI } else { -2.0 * PI } / span as f64;
        for start in (0..n).step_by(span) {
            for k in 0..span / 2 {
                let (sin, cos) = (step * k as f64).sin_cos();
                let (lo, hi) = (start + k, start + k + span / 2);
                let (ur, ui) = (re[lo], im[lo]);
                let vr = re[hi] * cos - im[hi] * sin;
                let vi = re[hi] * sin + im[hi] * cos;
                re[lo] = ur + vr;
                im[lo] = ui + vi;
                re[hi] = ur - vr;
                im[hi] = ui - vi;
            }
        }
        span <<= 1;
    }

    if inverse {
        let scale = 1.0 / n as f64;
        for x in re.iter_mut().chain(im.iter_mut()) {
            *x *= scale;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = 48_000.0;
    const SECONDS: f64 = 0.15;
    const MAX_LAG: usize = 4_800; // 100 ms

    fn noise(count: usize, level: f32, seed: u32) -> Vec<f32> {
        let mut state = seed | 1;
        (0..count)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((state >> 8) as f32 / (1 << 23) as f32 - 1.0) * level
            })
            .collect()
    }

    /// A room: the probe arrives `delay` samples late at `gain`, on top of
    /// whatever else is in the air.
    fn recorded(probe: &Probe, delay: usize, gain: f32, noise_level: f32) -> Vec<f32> {
        let mut room = noise(probe.needs(MAX_LAG) + delay, noise_level, 0x5eed);
        for (i, sample) in probe.signal().iter().enumerate() {
            room[delay + i] += sample * gain;
        }
        room
    }

    fn found(probe: &Probe, recording: &[f32]) -> Arrival {
        probe.find(recording, MAX_LAG).expect("nothing found")
    }

    /// Reads a couple of samples early and never late, because it is looking
    /// for the front of the arrival rather than its top and the peak of a band
    /// limited signal is a few samples wide. Three samples at 48 kHz is 60
    /// microseconds, against a delay handed to aec3 in whole milliseconds.
    #[test]
    fn a_delay_comes_back_within_a_sample_or_three() {
        let probe = Probe::new(RATE, SECONDS);
        for delay in [0, 137, 1_000, 2_400, 4_799] {
            let arrival = found(&probe, &recorded(&probe, delay, 1.0, 0.0));
            assert!(
                is_clear(arrival.confidence),
                "{delay}: {}",
                arrival.confidence
            );
            let out_by = arrival.delay_samples - delay as f64;
            assert!(
                (-3.0..=0.0).contains(&out_by),
                "asked for {delay}, got {}",
                arrival.delay_samples
            );
        }
    }

    /// 96 samples is 2 ms at 48 kHz, which is the accuracy the whole exercise
    /// is worth: aec3 is told the delay in whole milliseconds.
    #[test]
    fn noise_over_the_probe_does_not_move_the_peak() {
        let probe = Probe::new(RATE, SECONDS);
        let arrival = found(&probe, &recorded(&probe, 1_500, 1.0, 0.5));
        assert!(
            is_clear(arrival.confidence),
            "confidence {}",
            arrival.confidence
        );
        assert!(
            (arrival.delay_samples - 1_500.0).abs() <= 96.0,
            "got {}",
            arrival.delay_samples
        );
    }

    /// A quiet speaker across a room: the probe comes back 26 dB down, which
    /// puts it 12 dB under the noise it is buried in, and the matched filter
    /// still has to pull it out to the sample.
    #[test]
    fn a_probe_buried_under_the_room_is_still_found() {
        let probe = Probe::new(RATE, 0.4);
        let arrival = found(&probe, &recorded(&probe, 900, 0.05, 0.05));
        assert!(
            is_clear(arrival.confidence),
            "confidence {}",
            arrival.confidence
        );
        assert!(
            (arrival.delay_samples - 900.0).abs() <= 96.0,
            "got {}",
            arrival.delay_samples
        );
    }

    // The one that stops a broken run being reported as a number. Whatever the
    // peak search lands on in a recording with no probe in it, it must not come
    // back clear.
    #[test]
    fn a_recording_with_no_probe_in_it_is_never_clear() {
        let probe = Probe::new(RATE, SECONDS);
        for seed in [0x1234, 0x9e37, 0xabcd, 0x51ee] {
            let arrival = found(&probe, &noise(probe.needs(MAX_LAG), 0.3, seed));
            assert!(
                !is_clear(arrival.confidence),
                "noise came back clear at {}",
                arrival.confidence
            );
        }
    }

    #[test]
    fn silence_is_never_clear() {
        let probe = Probe::new(RATE, SECONDS);
        let arrival = probe.find(&vec![0.0; probe.needs(MAX_LAG)], MAX_LAG);
        assert!(arrival.is_none_or(|a| !is_clear(a.confidence)));
    }

    #[test]
    fn a_recording_that_stops_too_soon_is_not_a_measurement() {
        let probe = Probe::new(RATE, SECONDS);
        let short = recorded(&probe, 500, 1.0, 0.0);
        assert!(
            probe
                .find(&short[..probe.needs(MAX_LAG) - 1], MAX_LAG)
                .is_none()
        );
    }

    /// The Philips case: a sweep clocked out at 44.1 kHz and heard by a
    /// microphone running at 48. Nothing resamples anything; the sweep is a
    /// function of time, so the filter is simply generated at the rate the
    /// recording came in at. If that reasoning is wrong the peak smears and
    /// this stops finding the delay.
    #[test]
    fn a_sweep_played_at_one_rate_is_found_at_another() {
        let (played, heard) = (44_100.0, 48_000.0);
        let out = Probe::new(played, SECONDS);
        let listener = Probe::new(heard, SECONDS);

        let delay_ms = 203.0;
        let lag = (heard * delay_ms / 1000.0) as usize;
        let max_lag = (heard * 0.5) as usize;

        // The speaker's samples reconstructed on the microphone's clock, which
        // is all the air between them does to the signal.
        let mut room = vec![0.0f32; listener.needs(max_lag) + lag];
        for (i, room_sample) in room.iter_mut().enumerate().skip(lag) {
            let at = (i - lag) as f64 * played / heard;
            let (low, frac) = (at as usize, at.fract() as f32);
            match (out.signal().get(low), out.signal().get(low + 1)) {
                (Some(a), Some(b)) => *room_sample = (a * (1.0 - frac) + b * frac) * 0.4,
                _ => break,
            }
        }

        let arrival = listener.find(&room, max_lag).expect("nothing found");
        assert!(
            is_clear(arrival.confidence),
            "confidence {}",
            arrival.confidence
        );
        assert!(
            (arrival.delay_ms(heard) - delay_ms).abs() < 1.0,
            "asked for {delay_ms} ms, got {:.2}",
            arrival.delay_ms(heard)
        );
    }

    /// The two ends of the bridge do not have to agree on a sample rate, and
    /// the probe is generated once per rate rather than resampled.
    #[test]
    fn a_probe_works_at_whatever_rate_the_device_runs_at() {
        for rate in [44_100.0, 48_000.0, 96_000.0] {
            let probe = Probe::new(rate, SECONDS);
            let lag = (rate * 0.03) as usize;
            let max_lag = (rate * 0.1) as usize;
            let mut room = vec![0.0f32; probe.needs(max_lag) + lag];
            for (i, sample) in probe.signal().iter().enumerate() {
                room[lag + i] += sample * 0.5;
            }
            let arrival = probe.find(&room, max_lag).expect("nothing found");
            assert!(is_clear(arrival.confidence));
            assert!((arrival.delay_ms(rate) - 30.0).abs() < 1.0, "{rate} Hz");
        }
    }

    #[test]
    fn the_sweep_never_asks_the_speaker_for_more_than_it_was_told_to() {
        let probe = Probe::new(RATE, 0.4);
        assert!(probe.signal().iter().all(|s| s.abs() <= AMPLITUDE as f32));
        // Fading in means the first and last samples are effectively silent, so
        // starting or stopping it mid-callback cannot click.
        assert!(probe.signal()[0].abs() < 1e-4);
        assert!(probe.signal().last().unwrap().abs() < 1e-4);
    }

    #[test]
    fn the_transform_undoes_itself() {
        let mut re: Vec<f64> = (0..64).map(|i| (i as f64 * 0.37).sin()).collect();
        let mut im = vec![0.0; 64];
        let original = re.clone();
        fft(&mut re, &mut im, false);
        fft(&mut re, &mut im, true);
        for (got, want) in re.iter().zip(&original) {
            assert!((got - want).abs() < 1e-9);
        }
    }

    #[test]
    fn convolution_matches_the_direct_form() {
        let a: Vec<f32> = (0..40).map(|i| (i as f32 * 0.7).sin()).collect();
        let b: Vec<f64> = (0..13).map(|i| (i as f64 * 0.3).cos()).collect();
        let fast = convolve(&a, &b);
        assert_eq!(fast.len(), a.len() + b.len() - 1);
        for (n, got) in fast.iter().enumerate() {
            let want: f64 = (0..b.len())
                .filter(|k| n >= *k && n - k < a.len())
                .map(|k| a[n - k] as f64 * b[k])
                .sum();
            assert!((got - want).abs() < 1e-9, "at {n}: {got} vs {want}");
        }
    }
}
