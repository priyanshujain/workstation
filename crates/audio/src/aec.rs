//! The echo canceller, wrapped so the bridge can only use it correctly.
//!
//! Three things about aec3 0.3.2 are not negotiable, and all three are enforced
//! here rather than left to the caller:
//!
//! - aec3's own delay estimator runs the alignment, and the figure the bridge
//!   works out is only a starting hint. See below.
//! - The render reference must be mono. Given a stereo reference and a forced
//!   delay this version cancels almost nothing and invents its own delay
//!   instead, which is a bug in the port rather than in upstream AEC3.
//! - Frames are exactly 10 ms of interleaved f32, at the level Core Audio
//!   hands over, which is roughly -1 to 1. aec3's own thresholds are in int16
//!   units, so scaling the audio up to that range looks tempting and is wrong:
//!   past about ±1000 the matched filter reads every capture sample as
//!   saturated, stops updating, and the delay estimator goes blind.
//!
//! # Why the delay is a hint and not an instruction
//!
//! This used to force `use_external_delay_estimator`, which makes the supplied
//! delay authoritative. That was the wrong call, for a reason that only shows
//! up on real hardware: the Insta360 Link 2C redraws its buffering every time
//! its stream is opened. Six opens of the same speaker and microphone measured
//! 298, 300, 278, 286, 276 and 293 ms, stable within a run and never the same
//! between runs. A wired control microphone holds to a third of a millisecond
//! across the same test, so this is the device, not the measurement.
//!
//! aec3's adaptive filter spans 52 ms, so a supplied delay survives being about
//! 35 ms out and collapses beyond that. Measured on synthetic audio with the
//! echo at 320 ms: told 320 ms it reaches 23.2 dB, told 286 ms still 24.5 dB,
//! told 250 ms only 10.0 dB. A figure stored in a file therefore stops working
//! somewhere around the second or third time the device is opened, which is
//! exactly what the live bridge showed: 12.7 dB on a fresh measurement, 4.0 dB
//! on a stored one.
//!
//! aec3's internal estimator finds the same echo for itself in about four
//! seconds and re-finds it when it moves. Told nothing useful (a hint of 79 ms
//! against an echo at 320 ms) it still lands on 316 ms and 24.0 dB. Where a
//! stale supplied delay dies at 9.4 dB after the echo jumps from 280 ms to
//! 380 ms, the estimator comes back to 21.7 dB. It costs about a third of a
//! percent of one core.
//!
//! The default `num_filters` of 5 searches out to somewhere between 500 and
//! 600 ms, which is comfortably past anything this hardware produces, so it is
//! left alone. Widening it is possible (the range is 0..5000) and pointless:
//! 8 filters buys 600 ms, 12 buys 800 ms, at roughly a tenth of a percent of a
//! core each.
//!
//! The hint is still worth passing. It cannot pull the estimator off a delay it
//! has found, so a stale one does no harm, and it is the only thing standing in
//! when the far end is too quiet for the estimator to work at all.
//!
//! # What the hardware did
//!
//! All of the above was measured on synthetic noise. Thirteen opens of the
//! Insta360 against the built-in speaker, with speech at the far end, aligned
//! at 296, 312, 320, 320, 324, 324, 328, 332, 332, 340, 340, 344 and 344 ms
//! from a hint that never moved off 323 ms, and each open held its own figure
//! to within 8 ms for the length of the run. Four calibration runs over the
//! same pair measured 257.8, 262.0, 274.2 and 286.2 ms while agreeing with
//! themselves to 0.4 ms inside each open, and the wired control measured 65.5,
//! 64.5, 64.0 and 64.5 ms. So the device redraws and the estimator follows it.
//! ERLE over eight of those opens averaged 12.8 dB and stayed between 12.0 and
//! 13.2 dB, and what reached the far end was 50.8 dB below the echo the
//! microphone heard.
//!
//! Noise is not a usable far-end signal on this microphone, whatever the
//! synthetic work suggests. Band-limited pink noise at -18 dBFS reaches the
//! capture 26 dB down on speech at the same level, because the camera's own
//! suppressor eats it, and the canceller never aligns at all. Loud enough to
//! survive that and it saturates the capture instead, which blinds the
//! estimator for the reason given at the top of this file. Speech aligns
//! inside a second either way.

use aec3::api::EchoCanceller3Config;
use aec3::graph::{NodeControlState, NodeId};
use aec3::nodes::audio::AudioFormat;
use aec3::pipelines::linear::{self, LinearPipeline};
use anyhow::{Result, bail};

pub struct Canceller {
    pipeline: LinearPipeline,
    frame: usize,
    hint_ms: i32,
    found_delay_ms: i32,
    erle_db: f32,
    noise_suppressor: Option<NodeId>,
    suppressing: bool,
}

impl Canceller {
    /// Both streams are mono, and both must be fed one 10 ms frame at a time.
    /// The rates are independent, so a 44.1 kHz reference against a 48 kHz
    /// microphone is fine.
    ///
    /// `hint_ms` is where to start looking, not where the echo is.
    pub fn new(render_rate: u32, capture_rate: u32, hint_ms: i32) -> Result<Self> {
        let render = AudioFormat::ten_ms(render_rate, 1);
        let capture = AudioFormat::ten_ms(capture_rate, 1);

        let mut config = EchoCanceller3Config::default();
        // Set out loud rather than left to the default, because this is the one
        // line that decides whether the module documentation above holds.
        config.delay.use_external_delay_estimator = false;
        if !config.validate() {
            bail!("aec3 rejected its own default configuration");
        }

        let pipeline = linear::builder(render, capture)
            .aec3_config(config)
            .initial_delay_ms(hint_ms)
            .enable_high_pass_filter(true)
            .enable_noise_suppression(true)
            // The app at the far end does its own levelling, and a bridge that
            // quietly rides the gain is a surprise nobody asked for.
            .enable_gain_controller2(false)
            .export_metrics(true)
            .build()?;

        let noise_suppressor = pipeline.handles().noise_suppression.map(|ns| ns.node_id());
        Ok(Self {
            pipeline,
            frame: capture.sample_count(),
            hint_ms,
            found_delay_ms: 0,
            erle_db: 0.0,
            noise_suppressor,
            suppressing: true,
        })
    }

    /// Samples in one 10 ms frame of the capture stream.
    pub fn frame(&self) -> usize {
        self.frame
    }

    pub fn render(&mut self, frame: &[f32]) -> Result<()> {
        self.pipeline.handle_render_frame(frame)?;
        Ok(())
    }

    /// Cleans one capture frame. `false` means the pipeline has nothing to give
    /// back yet and `out` was left silent.
    pub fn capture(&mut self, frame: &[f32], out: &mut [f32]) -> Result<bool> {
        let produced = self.pipeline.process_capture_frame(frame, out)?;
        while let Some(packet) = self.pipeline.try_pull_metrics()? {
            let metrics = packet.payload();
            self.erle_db = metrics.echo_return_loss_enhancement as f32;
            self.found_delay_ms = metrics.delay_ms;
        }
        Ok(produced)
    }

    /// Where the canceller should start looking for the echo. Only sent on when
    /// it has actually moved, and it says whether it did.
    ///
    /// aec3 keeps this for the next time it rebuilds its alignment and ignores
    /// it the rest of the time, so a stale one cannot unseat a delay the
    /// estimator has already found.
    pub fn set_delay_hint_ms(&mut self, hint_ms: i32) -> Result<bool> {
        if hint_ms == self.hint_ms {
            return Ok(false);
        }
        self.pipeline.set_delay_ms(hint_ms)?;
        self.hint_ms = hint_ms;
        Ok(true)
    }

    /// aec3's own noise suppressor, which is bypassed rather than rebuilt so
    /// that switching it costs nothing the echo canceller has learned. Says
    /// whether it moved, like [`set_delay_hint_ms`].
    ///
    /// It earns its place while it is the only suppressor running and gets in
    /// the way the moment a better one does. Measured on speech at 9 dB SNR in
    /// white noise, with no echo: aec3 alone takes the noise floor down 8.4 dB,
    /// RNNoise alone 60.4 dB, and the two in series only 11.5 dB, because what
    /// aec3 leaves behind is no longer the stationary noise RNNoise was trained
    /// to find. Two suppressors are worse than either one.
    ///
    /// [`set_delay_hint_ms`]: Canceller::set_delay_hint_ms
    pub fn set_noise_suppression(&mut self, on: bool) -> Result<bool> {
        if on == self.suppressing {
            return Ok(false);
        }
        let Some(node) = self.noise_suppressor else {
            return Ok(false);
        };
        let state = if on {
            NodeControlState::Active
        } else {
            NodeControlState::Bypassed
        };
        self.pipeline.set_node_state(node, state)?;
        self.suppressing = on;
        Ok(true)
    }

    /// How much echo the canceller reckons it is removing, in dB.
    pub fn erle_db(&self) -> f32 {
        self.erle_db
    }

    /// Where the canceller has actually aligned its reference, in milliseconds.
    /// This is the number that matters: the hint is only a suggestion, and a
    /// figure near zero while audio is playing means the estimator has not
    /// found the echo.
    pub fn found_delay_ms(&self) -> i32 {
        self.found_delay_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;
    const FRAME: usize = 480;

    /// What the hardware actually produces, and further out than aec3's own
    /// 52 ms adaptive filter can reach without being aligned first.
    const FAR: usize = 320;

    /// A room: the echo is the far-end signal, quieter and later. `moves_at`
    /// is where the delay jumps to `then`, as it does when a device is closed
    /// and reopened partway through a call.
    fn echo(render: &[f32], delay: usize, then: usize, moves_at: usize, gain: f32) -> Vec<f32> {
        let mut out = vec![0.0; render.len()];
        for i in 0..render.len() {
            let d = if i < moves_at { delay } else { then } * RATE as usize / 1000;
            if i >= d {
                out[i] = render[i - d] * gain;
            }
        }
        out
    }

    fn noise(len: usize) -> Vec<f32> {
        let mut state = 0x9e3779b9u32;
        (0..len)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 8) as f32 / (1 << 23) as f32 - 1.0
            })
            .collect()
    }

    fn power(samples: &[f32]) -> f64 {
        samples.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / samples.len().max(1) as f64
    }

    struct Run {
        /// How much of the echo came out, in dB, over the second half.
        erle_db: f64,
        /// Where the canceller ended up aligned.
        found_ms: i32,
    }

    /// Plays `seconds` of noise at the canceller with its echo coming back at
    /// `delay_ms`, jumping to `then_ms` halfway through, and reports what
    /// happened. Nothing here touches a device.
    fn run(hint_ms: i32, seconds: usize, delay_ms: usize, then_ms: usize) -> Run {
        let samples = RATE as usize * seconds;
        let render = noise(samples);
        let capture = echo(&render, delay_ms, then_ms, samples / 2, 0.5);

        let mut canceller = Canceller::new(RATE, RATE, hint_ms).unwrap();
        let mut out = vec![0.0; FRAME];
        let (mut before, mut after) = (Vec::new(), Vec::new());

        for (i, (r, c)) in render.chunks(FRAME).zip(capture.chunks(FRAME)).enumerate() {
            canceller.render(r).unwrap();
            if canceller.capture(c, &mut out).unwrap() && i > seconds * 100 / 2 {
                // Only the second half, once the filter has converged.
                before.extend_from_slice(c);
                after.extend_from_slice(&out);
            }
        }
        Run {
            erle_db: 10.0 * (power(&before) / power(&after).max(1e-20)).log10(),
            found_ms: canceller.found_delay_ms(),
        }
    }

    /// A steady echo at `delay_ms` for `seconds`, told to start at `hint_ms`.
    fn steady(hint_ms: i32, seconds: usize, delay_ms: usize) -> Run {
        run(hint_ms, seconds, delay_ms, delay_ms)
    }

    // End to end: audio goes in and comes out quieter than it went in. Worth
    // keeping and worth not over-reading, because what comes out of the far end
    // of the chain has been through the residual echo suppressor, which gates
    // hard on synthetic noise whether or not the echo was ever aligned. It
    // reads about 41 dB here and about 90 dB with the echo somewhere the
    // canceller cannot reach, so it says the pipeline runs and nothing more.
    // Whether the echo was found is `the_canceller_finds_the_echo_for_itself`.
    #[test]
    fn the_echo_comes_out_quieter_than_it_went_in() {
        let erle = steady(100, 6, 100).erle_db;
        assert!(erle > 10.0, "only cancelled {erle:.1} dB");
    }

    // This replaces `the_wrong_delay_cancels_nothing`, which proved the same
    // thing the long way round: it forced a delay 300 ms out and watched
    // cancellation collapse, which only meant something while the supplied
    // delay was authoritative. Now that aec3 does its own searching, the
    // question is not whether it obeys but whether it looks, so this asks the
    // canceller directly where it aligned and refuses to accept the hint as an
    // answer. It fails if the external estimator is switched back on (the
    // alignment would sit at the 79 ms it was handed), if `num_filters` is cut
    // to nothing, if the audio is rescaled past the matched filter's
    // saturation limit, or if an aec3 upgrade regresses the estimator. The old
    // test caught only the first of those, and caught it through a proxy.
    #[test]
    fn the_canceller_finds_the_echo_for_itself() {
        let run = steady(79, 6, FAR);
        assert!(
            (run.found_ms - FAR as i32).abs() <= 24,
            "aligned at {} ms, not the {FAR} ms the echo is at",
            run.found_ms
        );
    }

    // The other half of the test above: proof that it is reporting a delay it
    // found rather than one it always reports. aec3's search reaches somewhere
    // between 500 and 600 ms at the default `num_filters`, so an echo at a
    // second is out of reach and has to come back unaligned. Unaligned means
    // the alignment sits near the 79 ms hint rather than anywhere near 1000,
    // which is what the threshold is drawn around.
    #[test]
    fn an_echo_beyond_the_search_window_is_never_found() {
        let run = steady(79, 6, 1000);
        assert!(
            run.found_ms < 100,
            "claims to have aligned at {} ms, which it cannot reach",
            run.found_ms
        );
    }

    // The whole point of the rewrite. The Insta360 draws a different delay
    // every time its stream is opened, so a delay that was right at the start
    // of a call is wrong by the middle of one, and the canceller has to follow
    // it without being told.
    #[test]
    fn a_delay_that_moves_mid_call_is_followed() {
        let run = run(280, 10, 280, 380);
        assert!(
            (run.found_ms - 380).abs() <= 24,
            "the echo moved to 380 ms and the canceller stayed at {} ms",
            run.found_ms
        );
    }

    #[test]
    fn a_frame_is_ten_milliseconds() {
        assert_eq!(Canceller::new(48_000, 48_000, 0).unwrap().frame(), 480);
        assert_eq!(Canceller::new(48_000, 44_100, 0).unwrap().frame(), 441);
    }

    // Mono on both sides, and the two rates independent of each other. A stereo
    // render is the one thing this version of aec3 silently gets wrong, so it
    // is worth an assertion rather than a comment.
    #[test]
    fn both_sides_are_mono() {
        let canceller = Canceller::new(44_100, 48_000, 0).unwrap();
        assert_eq!(
            canceller.pipeline.render_format(),
            AudioFormat::ten_ms(44_100, 1)
        );
        assert_eq!(
            canceller.pipeline.capture_format(),
            AudioFormat::ten_ms(48_000, 1)
        );
    }

    #[test]
    fn a_frame_of_the_wrong_size_is_refused() {
        let mut canceller = Canceller::new(RATE, RATE, 0).unwrap();
        assert!(canceller.render(&[0.0; FRAME - 1]).is_err());
        let mut out = vec![0.0; FRAME];
        assert!(canceller.capture(&[0.0; FRAME + 1], &mut out).is_err());
    }

    #[test]
    fn the_hint_only_moves_when_it_changes() {
        let mut canceller = Canceller::new(RATE, RATE, 100).unwrap();
        assert!(!canceller.set_delay_hint_ms(100).unwrap());
        assert!(canceller.set_delay_hint_ms(180).unwrap());
        assert!(!canceller.set_delay_hint_ms(180).unwrap());
    }

    // Nothing has been aligned before any audio has gone through, and saying
    // so is what lets the bridge tell "not found" apart from "found at zero".
    #[test]
    fn nothing_is_found_before_anything_is_played() {
        assert_eq!(Canceller::new(RATE, RATE, 320).unwrap().found_delay_ms(), 0);
    }
}
