//! Measures the real round trip from a speaker's IO proc to a microphone's.
//!
//! [`crate::bridge`] works its delay out from what Core Audio reports about
//! each device: latency, safety offset, stream latency, buffer size. That
//! figure has never been checked against a room, and for a Bluetooth speaker
//! there is good reason to think it is wrong, since macOS has claimed 82.5 ms
//! and 94 ms for the same speaker on two connections and real A2DP links run
//! 150 to 250 ms. With `use_external_delay_estimator` on, aec3 obeys whatever
//! it is told, so a wrong delay is not a degraded canceller, it is no canceller.
//!
//! What comes back is the whole path: the speaker's own latency, the flight
//! through the air, and the microphone's latency. That is deliberate. It is
//! exactly the interval aec3 needs, so no attempt is made to take the air back
//! out of it.
//!
//! It is worth the trouble. Running the bridge between the built-in speaker and
//! an Insta360 Link 2C, with speech playing through it: on the delay Core Audio
//! implies, 79 ms, the canceller managed 0.2 dB, which is to say nothing at all,
//! and held it there for the whole run. On the measured delay, 333 ms, the same
//! bridge on the same audio reached 12.7 dB and was still climbing.
//!
//! Two rules this measurement lives or dies by:
//!
//! - The real devices are opened directly, never ours. Playing into
//!   "Workstation Speaker" would measure our own loopback and hand back a
//!   confident number that had never been near a room, so the virtual UIDs are
//!   refused outright.
//! - The two ends need not share a sample rate, and against the Philips they do
//!   not: it runs at 44.1 kHz while the microphone runs at 48. The probe is a
//!   function of time, so it is generated once at each rate rather than
//!   resampled, and the recording is only ever measured in microphone samples.
//!
//! The measurement itself has been checked against a case where the right
//! answer is known rather than merely plausible. Pointed at our own virtual
//! Speaker and Speaker Tap, which are a digital loopback with one 512 frame
//! buffer between them and no room at all, it measured 515 samples, six
//! consecutive probes agreeing to within 0.1 ms. So the clock, the marks and
//! the deconvolution are good to a fraction of a millisecond, and a measurement
//! that comes back tens of milliseconds from what Core Audio claims is a
//! statement about the hardware and not about this code.
//!
//! That same loopback shows [`crate::bridge`] counting one buffer twice: Core
//! Audio reported 21.3 ms for a path that really takes 10.7, because the same
//! block is counted once on the way out and once on the way in when only one
//! exists. The correction absorbs it along with everything else, since both
//! sides of the subtraction are the same sum.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::correction::{Correction, now};
use crate::device::{self, Device, Direction};
use crate::probe::{self, Arrival, Probe};
use crate::ring;

/// How long one sweep lasts, and how long the room gets to itself afterwards.
///
/// The sweep length barely matters to the answer: at a fixed speaker and
/// microphone, 400, 250 and 150 ms sweeps measured 65.3, 64.3 and 64.0 ms with
/// much the same peak height, so the shorter end is taken for the one thing it
/// does buy, which is a shorter run. A Bluetooth speaker's latency drifts while
/// it is being measured, so every second the run does not take is a millisecond
/// of drift that is not in the spread.
///
/// The gap has to cover [`MAX_LAG_MS`] with room to spare, or the tail of one
/// probe is still arriving when the next one starts.
const SWEEP_MS: f64 = 250.0;
const GAP_MS: f64 = 600.0;

/// The furthest out an arrival is looked for. Comfortably past the worst A2DP
/// link, and a peak found near the far end of it is reported as a failure
/// rather than a delay, because that is what a search that found nothing and
/// settled on noise looks like.
const MAX_LAG_MS: f64 = 500.0;

/// Below this the recording cannot be of a speaker and a microphone in a room:
/// even the built-in pair is tens of milliseconds apart. A peak here means the
/// probe leaked back electrically, or the maths is wrong.
const MIN_PLAUSIBLE_MS: f64 = 2.0;

/// How far apart the probes may land before the run is a failure rather than an
/// answer. One bad estimate hiding inside a median is exactly the silent wrong
/// number this whole command exists to catch. Set from what a fixed speaker and
/// microphone on a desk actually do, which is 4 to 5 ms of honest wander between
/// probes, plus the few more a Bluetooth speaker adds by drifting about 0.6 ms
/// per second while it is being measured. It is set well below the failures it
/// has to catch: an unsettled A2DP link scattered its probes over 38 ms, and
/// readings driven by noise rather than the probe land hundreds of ms apart.
const AGREEMENT_MS: f64 = 12.0;

/// Probes played and thrown away before any are believed.
///
/// A Bluetooth speaker's latency is not a constant, it is a buffer filling up.
/// Measured against the Philips from a cold link the first probe came back at
/// 70 ms, the second at 75, and only from the fourth on did it settle at 105 to
/// 108. Every one of those had a confident, unambiguous peak, so no confidence
/// check could have caught it: the early ones are the honest latency of a link
/// that has not finished starting, and the answer wanted here is the one it
/// holds afterwards. It took a little over three seconds to settle, and a wired
/// device pays only the time.
const WARMUP_PROBES: usize = 4;

/// How many probes have to come back with a clear peak for the run to count.
const MIN_CLEAR: usize = 3;

/// Room for the warm-up as well as the probes that are kept.
const MAX_MARKS: usize = 24;
const MAX_PROBES: usize = MAX_MARKS - WARMUP_PROBES;
const DEFAULT_PROBES: usize = 6;

/// The largest block either device is expected to hand a callback.
const MAX_BLOCK_FRAMES: usize = 8192;

/// Nothing is playing. Not a position, so it cannot be confused with one.
const IDLE: usize = usize::MAX;

/// One probe's worth of answer.
#[derive(Clone, Copy, Debug)]
pub struct Reading {
    pub delay_ms: f64,
    pub confidence: f64,
}

impl Reading {
    pub fn is_clear(&self) -> bool {
        probe::is_clear(self.confidence)
    }
}

/// What a run measured, and what the bridge would have guessed instead.
#[derive(Clone, Debug)]
pub struct Calibration {
    pub output_uid: String,
    pub output_name: String,
    pub output_rate: f64,
    pub input_uid: String,
    pub input_name: String,
    pub input_rate: f64,
    /// Every probe, clear or not, in the order they were played.
    pub readings: Vec<Reading>,
    /// The measured round trip: speaker IO proc, through the air, back to the
    /// microphone IO proc.
    pub measured_ms: f64,
    /// The same interval as the bridge computes it from what Core Audio
    /// reports, which is what the measurement is being checked against.
    pub computed_ms: f64,
    /// Widest disagreement between the probes that were believed.
    pub spread_ms: f64,
    /// The weakest peak among them, so the number quoted is the worst case
    /// rather than the flattering one.
    pub confidence: f64,
}

/// Metres sound covers in a millisecond, near enough for a room.
const METRES_PER_MS: f64 = 0.343;

/// Beyond this much unexplained delay, no room could account for it and the
/// rest has to be buffering inside a device. About 3.4 m of air, which is
/// further apart than a speaker and a microphone on one desk ever are.
const AIR_MS: f64 = 10.0;

impl Calibration {
    /// What Core Audio is out by. Positive means it under-reported, which for a
    /// Bluetooth speaker means the bridge has been cancelling at an offset the
    /// echo never arrives at.
    pub fn difference_ms(&self) -> f64 {
        self.measured_ms - self.computed_ms
    }

    /// How far apart the two devices would have to be for the difference to be
    /// flight time through the air. Worth printing whenever it is large, since
    /// a number like 254 ms invites being read as an implausible distance when
    /// what it really is is buffering nobody declared.
    pub fn difference_as_air_metres(&self) -> f64 {
        self.difference_ms() * METRES_PER_MS
    }

    /// Whether the difference is past anything the room could explain, so the
    /// rest is latency inside a device that Core Audio does not report.
    pub fn is_beyond_air(&self) -> bool {
        self.difference_ms().abs() > AIR_MS
    }

    pub fn correction(&self) -> Correction {
        Correction {
            output_uid: self.output_uid.clone(),
            input_uid: self.input_uid.clone(),
            measured_ms: self.measured_ms,
            computed_ms: self.computed_ms,
            confidence: self.confidence,
            measured_at: now(),
        }
    }
}

/// Plays the probe out of `output_uid` and listens for it on `input_uid`.
/// Takes about `probes` times a second and a half, and makes a noise.
pub fn run(input_uid: &str, output_uid: &str, probes: usize) -> Result<Calibration> {
    let probes = probes.clamp(1, MAX_PROBES);
    refuse_virtual(output_uid)?;
    refuse_virtual(input_uid)?;

    let speaker = device::open(output_uid, Direction::Output)
        .with_context(|| format!("could not open the speaker {output_uid}"))?;
    let microphone = device::open(input_uid, Direction::Input)
        .with_context(|| format!("could not open the microphone {input_uid}"))?;

    // Two probes of the same sweep, one per clock. The signal is a function of
    // time, so this is the same sound generated twice, not a resampling.
    let played = Probe::new(speaker.rate, SWEEP_MS / 1000.0);
    let heard = Probe::new(microphone.rate, SWEEP_MS / 1000.0);

    let max_lag = (microphone.rate * MAX_LAG_MS / 1000.0) as usize;
    let rounds = WARMUP_PROBES + probes;
    let seconds = rounds as f64 * (SWEEP_MS + GAP_MS) / 1000.0 + 2.0;
    let recording = ring::ring((microphone.rate * seconds) as usize);

    let session = Arc::new(Session::new(played.signal().to_vec(), microphone.rate));
    let listening = listen(&microphone, &session, &recording)?;

    // The output side reads the microphone's clock to mark when each sweep
    // starts, so there has to be one before anything is played.
    session
        .wait_for_microphone(Duration::from_secs(3))
        .context("the microphone delivered nothing, so there was nothing to measure against")?;
    let playing = play(&speaker, &session)?;

    let mut recorded: Vec<f32> = Vec::with_capacity((microphone.rate * seconds) as usize);
    let mut consumer = recording.consumer().expect("one reader");
    let mut block = vec![0.0f32; MAX_BLOCK_FRAMES];

    for probe in 1..=rounds {
        session.arm.store(probe, Ordering::Release);
        let until = Instant::now() + Duration::from_millis((SWEEP_MS + GAP_MS) as u64);
        while Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
            drain(&mut consumer, &mut block, &mut recorded);
        }
    }
    drop(playing);
    drain(&mut consumer, &mut block, &mut recorded);
    drop(listening);
    drain(&mut consumer, &mut block, &mut recorded);

    // A run that comes back with nothing is nearly always the level rather than
    // the maths, so how loud the room was is worth having at -vv.
    tracing::debug!(
        frames = recorded.len(),
        peak = recorded.iter().fold(0.0f32, |m, s| m.max(s.abs())),
        rms = (recorded.iter().map(|s| (*s as f64).powi(2)).sum::<f64>()
            / recorded.len().max(1) as f64)
            .sqrt(),
        "recorded"
    );

    // A ring that overflowed means the recording has a hole in it, and every
    // sample index after the hole points at the wrong moment. Nothing about the
    // run is salvageable.
    if recording.lost() > 0 {
        bail!(
            "{} microphone samples were dropped while recording, so the timing is meaningless",
            recording.lost()
        );
    }

    let readings = read_probes(
        &session,
        &heard,
        &recorded,
        max_lag,
        WARMUP_PROBES..rounds,
        microphone.rate,
    );
    let computed_ms = reported_ms(&speaker, &microphone);
    settle(Calibration {
        output_uid: speaker.uid,
        output_name: speaker.name,
        output_rate: speaker.rate,
        input_uid: microphone.uid,
        input_name: microphone.name,
        input_rate: microphone.rate,
        readings,
        measured_ms: 0.0,
        computed_ms,
        spread_ms: 0.0,
        confidence: 0.0,
    })
}

/// The sum the bridge makes of the two devices' reported latencies, which is
/// the half of its delay the room has anything to say about. Kept beside the
/// measurement so the two numbers being compared are unarguably the same thing.
fn reported_ms(speaker: &Device, microphone: &Device) -> f64 {
    speaker.latency_ms() + microphone.latency_ms()
}

/// Turns the probes into an answer, or says why there isn't one. Every way out
/// of here that is not a measurement is an error, never a number.
fn settle(mut calibration: Calibration) -> Result<Calibration> {
    let mut clear: Vec<f64> = calibration
        .readings
        .iter()
        .filter(|r| r.is_clear())
        .map(|r| r.delay_ms)
        .collect();

    // Told apart on purpose. Weak peaks mean the probe was heard badly; no
    // readings at all means it was never heard, or never played, which is a
    // different thing to go and check.
    if calibration.readings.is_empty() {
        bail!(
            "not one probe came back: nothing was recorded where the sweeps should have been. \
             Check the speaker is not muted, and that the microphone is the one in the room"
        );
    }
    if clear.len() < MIN_CLEAR.min(calibration.readings.len()) {
        bail!(
            "only {} of {} probes came back with a clear peak, so nothing was measured. \
             Turn the speaker up a little, move the microphone nearer, or quieten the room",
            clear.len(),
            calibration.readings.len()
        );
    }
    clear.sort_by(f64::total_cmp);

    calibration.measured_ms = clear[clear.len() / 2];
    calibration.spread_ms = clear[clear.len() - 1] - clear[0];
    calibration.confidence = calibration
        .readings
        .iter()
        .filter(|r| r.is_clear())
        .map(|r| r.confidence)
        .fold(f64::INFINITY, f64::min);

    if calibration.spread_ms > AGREEMENT_MS {
        bail!(
            "the probes disagree by {:.1} ms, from {:.1} to {:.1}, so none of them is trustworthy",
            calibration.spread_ms,
            clear[0],
            clear[clear.len() - 1]
        );
    }
    if calibration.measured_ms < MIN_PLAUSIBLE_MS {
        bail!(
            "measured {:.1} ms, which is too short to have crossed a room. \
             Check the probe is going to the real speaker and not back on itself",
            calibration.measured_ms
        );
    }
    if calibration.measured_ms > MAX_LAG_MS * 0.95 {
        bail!(
            "measured {:.1} ms, at the far end of the {MAX_LAG_MS:.0} ms searched, \
             which is what a search that found nothing looks like",
            calibration.measured_ms
        );
    }
    Ok(calibration)
}

/// Deconvolves the window after each mark. A probe whose mark never landed, or
/// whose window ran off the end of the recording, is dropped rather than
/// guessed at, and [`settle`] decides whether enough are left.
fn read_probes(
    session: &Session,
    heard: &Probe,
    recorded: &[f32],
    max_lag: usize,
    kept: std::ops::Range<usize>,
    rate: f64,
) -> Vec<Reading> {
    kept.filter_map(|i| {
        let mark = f64::from_bits(session.marks[i].load(Ordering::Acquire));
        if !mark.is_finite() || mark < 0.0 {
            tracing::warn!("probe {} never started", i + 1);
            return None;
        }
        let from = mark as usize;
        let window = recorded.get(from..)?;
        let arrival = heard.find(window, max_lag)?;
        tracing::debug!(
            probe = i + 1,
            mark,
            window = window.len(),
            delay_samples = arrival.delay_samples,
            confidence = arrival.confidence,
            "probe"
        );

        // The mark falls between two microphone samples, and the window
        // could only start at one of them.
        let arrival = Arrival {
            delay_samples: arrival.delay_samples - (mark - from as f64),
            ..arrival
        };
        Some(Reading {
            delay_ms: arrival.delay_ms(rate),
            confidence: arrival.confidence,
        })
    })
    .collect()
}

fn refuse_virtual(uid: &str) -> Result<()> {
    if uid.starts_with("WSSpeaker") || uid.starts_with("WSMicrophone") {
        bail!(
            "{uid} is one of the wsctl virtual devices. Calibrating against it would measure \
             the bridge's own loopback rather than the room, so pick the real hardware: \
             wsctl audio devices"
        );
    }
    Ok(())
}

fn drain(consumer: &mut ring::Consumer, block: &mut [f32], into: &mut Vec<f32>) {
    loop {
        let n = consumer.pop(block);
        if n == 0 {
            return;
        }
        into.extend_from_slice(&block[..n]);
    }
}

/// Everything the two callbacks share. Atomics only: the microphone's clock is
/// read from the speaker's IO thread, which must not be made to wait for it.
struct Session {
    signal: Vec<f32>,
    /// How far into `signal` the speaker has got, or [`IDLE`].
    position: AtomicUsize,
    /// One-based index of the probe the main thread wants next, 0 for none.
    arm: AtomicUsize,
    /// For each probe, where the microphone had got to at the instant its first
    /// sample went to the speaker, in microphone frames, as f64 bits.
    marks: [AtomicU64; MAX_MARKS],
    /// Microphone frames delivered in the high half, microseconds since
    /// `epoch` in the low half. One word so the speaker's callback cannot read
    /// a count from one microphone block and a time from the next.
    clock: AtomicU64,
    epoch: Instant,
    input_rate: f64,
}

impl Session {
    fn new(signal: Vec<f32>, input_rate: f64) -> Self {
        Session {
            signal,
            position: AtomicUsize::new(IDLE),
            arm: AtomicUsize::new(0),
            marks: [const { AtomicU64::new(f64::NAN.to_bits()) }; MAX_MARKS],
            clock: AtomicU64::new(0),
            epoch: Instant::now(),
            input_rate,
        }
    }

    /// Where the microphone has got to right now, in frames, interpolated
    /// between its callbacks. Without the interpolation the answer would be
    /// quantised to a whole microphone block, which at 512 frames is a 10 ms
    /// bias on a measurement worth a millisecond or two.
    ///
    /// Called from the speaker's IO thread. Reading the clock there is a
    /// vDSO-backed load with nothing to block on, which is fine for a
    /// calibration run and is not something the bridge does.
    fn microphone_now(&self) -> f64 {
        let clock = self.clock.load(Ordering::Acquire);
        if clock == 0 {
            return f64::NAN;
        }
        let frames = (clock >> 32) as f64;
        let then = (clock & 0xffff_ffff) as f64;
        let now = self.epoch.elapsed().as_micros() as f64;
        frames + (now - then) * 1e-6 * self.input_rate
    }

    fn note_microphone(&self, frames: u64) {
        let micros = self.epoch.elapsed().as_micros() as u64 & 0xffff_ffff;
        self.clock.store((frames << 32) | micros, Ordering::Release);
    }

    fn wait_for_microphone(&self, timeout: Duration) -> Result<()> {
        let until = Instant::now() + timeout;
        while Instant::now() < until {
            if self.clock.load(Ordering::Acquire) != 0 {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        bail!("no microphone block arrived within {timeout:?}");
    }
}

/// Records the microphone, mono, and keeps the clock the speaker's side reads.
fn listen(
    device: &Device,
    session: &Arc<Session>,
    recording: &Arc<ring::Shared>,
) -> Result<device::Stream> {
    let mut producer = recording.producer().expect("one writer");
    let channels = device.channels;
    let mut mono = vec![0.0f32; MAX_BLOCK_FRAMES];
    let mut delivered = 0u64;
    let session = session.clone();

    device::Stream::input(device, move |samples| {
        let frames = samples.len() / channels;
        if frames > mono.len() {
            return;
        }
        let scale = 1.0 / channels as f32;
        for (frame, block) in mono[..frames].iter_mut().zip(samples.chunks(channels)) {
            *frame = block.iter().sum::<f32>() * scale;
        }
        // In the ring before it is announced, so the count the speaker's side
        // reads never runs ahead of what has actually been recorded.
        producer.push(&mono[..frames]);
        delivered += frames as u64;
        session.note_microphone(delivered);
    })
}

/// Plays silence, and a sweep whenever the main thread asks for one. A sweep
/// always starts at sample zero of a block, so the moment it is marked is the
/// moment its first sample is handed over, with no offset into the buffer to
/// account for afterwards.
fn play(device: &Device, session: &Arc<Session>) -> Result<device::Stream> {
    let channels = device.channels;
    let session = session.clone();

    device::Stream::output(device, move |out| {
        out.fill(0.0);
        let mut position = session.position.load(Ordering::Relaxed);
        if position == IDLE {
            let probe = session.arm.swap(0, Ordering::AcqRel);
            if probe == 0 {
                return;
            }
            session.marks[probe - 1].store(session.microphone_now().to_bits(), Ordering::Release);
            position = 0;
        }

        let take = (out.len() / channels).min(session.signal.len() - position);
        for (frame, sample) in out
            .chunks_mut(channels)
            .zip(&session.signal[position..position + take])
        {
            frame.fill(*sample);
        }

        position += take;
        session.position.store(
            if position >= session.signal.len() {
                IDLE
            } else {
                position
            },
            Ordering::Relaxed,
        );
    })
}

pub fn default_probes() -> usize {
    DEFAULT_PROBES
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calibration(readings: &[(f64, f64)]) -> Calibration {
        Calibration {
            output_uid: "30-21-39-26-64-C7:output".into(),
            output_name: "Philips MMS2625B".into(),
            output_rate: 44_100.0,
            input_uid: "AppleUSBAudioEngine:Insta360:Insta360 Link 2C:111000:3".into(),
            input_name: "Insta360 Link 2C".into(),
            input_rate: 48_000.0,
            readings: readings
                .iter()
                .map(|(delay_ms, confidence)| Reading {
                    delay_ms: *delay_ms,
                    confidence: *confidence,
                })
                .collect(),
            measured_ms: 0.0,
            computed_ms: 108.0,
            spread_ms: 0.0,
            confidence: 0.0,
        }
    }

    #[test]
    fn probes_that_agree_give_the_middle_one() {
        let settled = settle(calibration(&[
            (203.1, 22.0),
            (202.4, 31.0),
            (203.0, 18.0),
            (202.9, 27.0),
        ]))
        .unwrap();
        assert_eq!(settled.measured_ms, 203.0);
        assert!((settled.spread_ms - 0.7).abs() < 1e-9);
        assert_eq!(settled.confidence, 18.0);
        assert!((settled.difference_ms() - 95.0).abs() < 1e-9);
    }

    // Weak peaks are not votes. Three good probes and three that found nothing
    // is still a measurement, made only of the three that worked.
    #[test]
    fn probes_that_found_nothing_are_left_out_rather_than_averaged_in() {
        let settled = settle(calibration(&[
            (41.0, 61.0),
            (411.0, 4.2),
            (41.2, 55.0),
            (7.0, 4.1),
            (41.1, 48.0),
            (300.0, 5.0),
        ]))
        .unwrap();
        assert_eq!(settled.measured_ms, 41.1);
        assert_eq!(settled.readings.len(), 6);
        // The number quoted is the worst of the ones that counted, not the
        // best, and not the ones that were thrown out.
        assert_eq!(settled.confidence, 48.0);
    }

    #[test]
    fn too_few_clear_probes_is_a_failure_not_a_number() {
        let error = settle(calibration(&[
            (41.0, 55.0),
            (120.0, 4.1),
            (330.0, 4.4),
            (9.0, 5.0),
            (250.0, 4.9),
            (41.2, 11.9),
        ]))
        .unwrap_err();
        assert!(format!("{error}").contains("1 of 6"), "{error}");
    }

    // Six confident answers that do not agree with each other are six answers
    // to different questions, and the median of them is a fiction.
    #[test]
    fn probes_that_disagree_are_a_failure_not_a_median() {
        let error = settle(calibration(&[
            (41.0, 32.0),
            (63.0, 44.0),
            (52.0, 29.0),
            (44.0, 20.0),
        ]))
        .unwrap_err();
        assert!(
            format!("{error}").contains("disagree by 22.0 ms"),
            "{error}"
        );
    }

    #[test]
    fn a_round_trip_of_nothing_is_a_failure() {
        let error = settle(calibration(&[(0.4, 30.0), (0.4, 28.0), (0.5, 25.0)])).unwrap_err();
        assert!(format!("{error}").contains("too short"), "{error}");
    }

    #[test]
    fn a_peak_at_the_end_of_the_search_is_a_failure() {
        let error =
            settle(calibration(&[(495.0, 30.0), (494.0, 28.0), (496.0, 25.0)])).unwrap_err();
        assert!(format!("{error}").contains("far end"), "{error}");
    }

    // The trap the whole command is written around. Measuring through our own
    // devices would give a tight, confident, entirely fictional number.
    #[test]
    fn the_virtual_devices_are_refused() {
        for uid in ["WSSpeaker_UID", "WSSpeaker_2_UID", "WSMicrophone_2_UID"] {
            let error = refuse_virtual(uid).unwrap_err();
            assert!(format!("{error}").contains("loopback"), "{uid}");
        }
        assert!(refuse_virtual("BuiltInSpeakerDevice").is_ok());
        assert!(refuse_virtual("30-21-39-26-64-C7:output").is_ok());
    }

    #[test]
    fn calibrating_a_virtual_device_never_opens_anything() {
        assert!(run("BuiltInMicrophoneDevice", "WSSpeaker_UID", 1).is_err());
        assert!(run("WSMicrophone_UID", "BuiltInSpeakerDevice", 1).is_err());
    }

    // A difference of a couple of hundred milliseconds is not a room, and the
    // report has to say so rather than leave it to be read as one.
    #[test]
    fn a_difference_too_big_for_the_room_is_called_out() {
        let insta = settle(calibration(&[
            (296.0, 104.0),
            (295.0, 98.0),
            (297.0, 120.0),
        ]))
        .unwrap();
        assert!(insta.is_beyond_air());
        // 188 ms past what Core Audio claims would be 64 m of air, which is
        // a desk it plainly is not on.
        assert!((insta.difference_as_air_metres() - 64.5).abs() < 0.5);

        // A speaker and a microphone on one desk, where the difference really
        // is the air and nothing needs explaining.
        let desk = settle(calibration(&[(110.0, 40.0), (110.5, 44.0), (109.8, 39.0)])).unwrap();
        assert!(!desk.is_beyond_air());
    }

    #[test]
    fn what_is_saved_is_what_was_measured() {
        let settled = settle(calibration(&[(203.0, 22.0), (203.0, 31.0), (203.0, 18.0)])).unwrap();
        let correction = settled.correction();
        assert_eq!(correction.output_uid, settled.output_uid);
        assert_eq!(correction.input_uid, settled.input_uid);
        assert_eq!(correction.offset_ms(), settled.difference_ms());
        assert_eq!(correction.confidence, 18.0);
    }

    /// A mark that never landed, because the speaker was gone by then, drops
    /// that probe instead of pointing the window at sample zero.
    #[test]
    fn a_probe_that_never_started_is_dropped() {
        let session = Session::new(vec![0.0; 16], 48_000.0);
        let heard = Probe::new(48_000.0, 0.05);
        let recorded = vec![0.0f32; 48_000];
        assert!(read_probes(&session, &heard, &recorded, 4_800, 0..3, 48_000.0).is_empty());
    }

    // Nothing at all is a different complaint from something heard badly, and
    // it sends you to look somewhere else.
    #[test]
    fn a_run_that_heard_nothing_says_so() {
        let error = settle(calibration(&[])).unwrap_err();
        assert!(
            format!("{error}").contains("not one probe came back"),
            "{error}"
        );
    }

    /// The warm-up probes are played and thrown away, and the ones that are
    /// kept are read against their own marks. Built here out of a recording
    /// with the probe planted at a known lag after each mark, including a mark
    /// that falls between two samples, which is the correction that stops the
    /// answer being quantised to a whole microphone block.
    #[test]
    fn the_warm_up_probes_are_played_and_not_counted() {
        const RATE: f64 = 48_000.0;
        let heard = Probe::new(RATE, 0.05);
        let max_lag = 4_800;
        let lag = 1_440; // 30 ms
        let session = Session::new(Vec::new(), RATE);

        let stride = heard.needs(max_lag) + lag + 1_000;
        let mut recorded = vec![0.0f32; stride * 5];
        for probe in 0..5 {
            let mark = (probe * stride) as f64 + 0.5;
            session.marks[probe].store(mark.to_bits(), Ordering::Release);
            for (i, sample) in heard.signal().iter().enumerate() {
                recorded[probe * stride + lag + i] = *sample;
            }
        }

        let kept = read_probes(&session, &heard, &recorded, max_lag, 2..5, RATE);
        assert_eq!(kept.len(), 3, "warm-up probes were counted");
        for reading in kept {
            assert!(reading.is_clear());
            assert!(
                (reading.delay_ms - 30.0).abs() < 0.2,
                "got {}",
                reading.delay_ms
            );
        }
    }

    #[test]
    fn the_microphone_clock_packs_and_unpacks() {
        let session = Session::new(Vec::new(), 48_000.0);
        assert!(session.microphone_now().is_nan());
        assert!(
            session
                .wait_for_microphone(Duration::from_millis(30))
                .is_err()
        );

        session.note_microphone(123_456);
        assert!(
            session
                .wait_for_microphone(Duration::from_millis(30))
                .is_ok()
        );
        let position = session.microphone_now();
        assert!(
            (123_456.0..123_456.0 + 4_800.0).contains(&position),
            "got {position}"
        );
    }
}
