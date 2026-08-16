//! The echo-cancelling bridge between the virtual devices and the real ones.
//!
//! ```text
//! apps -> "Workstation Speaker" -> [ring] -> "Workstation Speaker Tap" -.-> real speaker
//!                                                            '-> AEC reference
//! real microphone -> AEC capture -> "Workstation Mic Feed" -> [ring] -> "Workstation Mic" -> apps
//! ```
//!
//! Three clocks meet here and none of them agree: the virtual devices run off
//! the host clock, the microphone off whatever crystal is in it, and a
//! Bluetooth speaker off its own idea of 44.1 kHz. Every hop between them is a
//! lock-free ring with a resampler on the far end that watches how full the
//! ring is and bends its ratio to hold it there. The IO callbacks themselves
//! only copy, downmix and resample: no allocation, no locks, no logging.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::{JoinHandle, Thread};
use std::time::Duration;

use anyhow::{Context, Result};

use crate::aec::Canceller;
use crate::correction::{self, Corrections};
use crate::denoise::Denoiser;
use crate::device::{self, Device, Direction, Stream};
use crate::resample::{Loss, Resampler};
use crate::ring;

/// The two virtual devices the bridge itself owns. The other two belong to the
/// apps at either end and the bridge never touches them.
const TAP_UID: &str = "WSSpeaker_2_UID";
const FEED_UID: &str = "WSMicrophone_2_UID";

/// How much audio each ring aims to hold. It buys the far end room to be late
/// without starving, and it is paid for in latency, so it is deliberately mean.
const TARGET_MS: f64 = 20.0;
const CAPACITY_MS: f64 = 500.0;

/// The largest block any device is expected to hand a callback. Anything
/// bigger is dropped rather than allocated for.
const MAX_BLOCK_FRAMES: usize = 8192;

/// How many 10 ms frames of reference the canceller will let pile up before it
/// throws the backlog away and carries on from the newest.
const BACKLOG_FRAMES: usize = 5;

static ENGINE: Mutex<Option<Engine>> = Mutex::new(None);
static RUNNING: AtomicBool = AtomicBool::new(false);

// Settings rather than engine state, so they live out here with RUNNING: the
// bridge is torn down and rebuilt every time somebody picks a different
// microphone, and a noise suppressor that switched itself off each time would
// be its own bug report. Both are read once per 10 ms frame by the worker.
static DENOISE: AtomicBool = AtomicBool::new(false);
static VOICE_THRESHOLD: AtomicU32 = AtomicU32::new(0);

/// Starts the bridge between `input_uid` (a real microphone) and `output_uid`
/// (a real speaker). Starting a bridge that is already up replaces it.
pub fn start(input_uid: &str, output_uid: &str) -> Result<()> {
    // Held across the teardown as well as the setup, so a start racing a stop
    // cannot have two engines reaching for the same devices.
    let mut slot = lock();
    drop(slot.take());
    RUNNING.store(false, Ordering::SeqCst);

    *slot = Some(Engine::start(input_uid, output_uid)?);
    RUNNING.store(true, Ordering::SeqCst);
    tracing::info!(input_uid, output_uid, "bridge running");
    Ok(())
}

/// Stops the bridge and releases every device. Safe to call when nothing is
/// running, and safe to call twice.
pub fn stop() -> Result<()> {
    let mut slot = lock();
    RUNNING.store(false, Ordering::SeqCst);
    drop(slot.take());
    Ok(())
}

pub fn is_running() -> bool {
    RUNNING.load(Ordering::SeqCst)
}

/// Whether the microphone is being run through RNNoise. Off by default.
pub fn denoise() -> bool {
    DENOISE.load(Ordering::Relaxed)
}

/// Turns noise suppression on or off. It takes hold within a frame or two,
/// mid-call, and switching it off leaves the audio exactly as the echo
/// canceller left it rather than processing it and throwing the result away.
pub fn set_denoise(on: bool) {
    DENOISE.store(on, Ordering::Relaxed);
}

/// The voice probability below which the microphone is faded out. Zero, the
/// default, leaves the gate open.
pub fn voice_threshold() -> f32 {
    f32::from_bits(VOICE_THRESHOLD.load(Ordering::Relaxed))
}

/// Sets the voice gate, clamped to 0.0 ..= 1.0. Only does anything while
/// [`set_denoise`] is on, since the probability comes out of the model.
pub fn set_voice_threshold(threshold: f32) {
    let threshold = if threshold.is_nan() {
        0.0
    } else {
        threshold.clamp(0.0, 1.0)
    };
    VOICE_THRESHOLD.store(threshold.to_bits(), Ordering::Relaxed);
}

/// What the bridge has moved since it started, for anything that wants to show
/// or check on it. `None` when nothing is running.
pub fn stats() -> Option<Stats> {
    lock().as_ref().map(|engine| engine.shared.stats())
}

#[derive(Debug, Clone, Copy)]
pub struct Stats {
    pub tap_frames: u64,
    pub speaker_frames: u64,
    pub microphone_frames: u64,
    pub feed_frames: u64,
    /// Frames that had to be invented because a ring ran dry, or dropped
    /// because it ran over. Anything but zero means a clock is getting away
    /// from us.
    pub glitched: u64,
    /// Frames thrown away to pull a ring back to its target. A virtual device
    /// hands over a burst when it starts, so this is never zero for long, and
    /// it only matters if it keeps climbing.
    pub resynced: u64,
    /// Where the bridge suggests the canceller starts looking for the echo.
    /// Only a starting point: aec3 searches for itself and this cannot pull it
    /// off what it finds.
    pub delay_ms: i32,
    /// Where the canceller actually aligned its reference. This is the delay
    /// that is doing the work. Zero means it has not found the echo, which is
    /// what silence looks like as well as what failure looks like.
    pub found_delay_ms: i32,
    /// Whether the starting point was measured against this pair of devices by
    /// `wsctl audio calibrate`, or is only what Core Audio claims.
    pub calibrated: bool,
    /// Whether that measurement is old enough that it should not be quoted as
    /// describing the hardware as it is now. It is still used as a starting
    /// point, which is all any measurement is worth here.
    pub stale_calibration: bool,
    /// How much of `delay_ms` came from the measurement rather than from what
    /// the devices reported. Zero when nothing has been measured.
    pub correction_ms: f64,
    pub erle_db: f32,
    /// Whether RNNoise is running on the microphone.
    pub denoising: bool,
    /// How likely the model thought the last frame was to be speech, from 0 to
    /// 1. Zero whenever the suppressor is off.
    pub voice: f32,
    /// How far the speaker's resampler is bending its ratio to stop the render
    /// ring drifting, in parts per million.
    pub speaker_drift_ppm: f64,
    pub speaker_attached: bool,
    pub microphone_attached: bool,
}

fn lock() -> std::sync::MutexGuard<'static, Option<Engine>> {
    // A panic elsewhere must not leave the bridge unstoppable.
    ENGINE.lock().unwrap_or_else(|e| e.into_inner())
}

struct Engine {
    shared: Arc<Shared>,
    endpoints: Arc<Mutex<Vec<Endpoint>>>,
    worker: Option<JoinHandle<()>>,
    supervisor: Option<JoinHandle<()>>,
}

impl Engine {
    fn start(input_uid: &str, output_uid: &str) -> Result<Self> {
        let tap = device::open(TAP_UID, Direction::Input)
            .context("the wsctl virtual devices are missing, run: wsctl audio install")?;
        let feed = device::open(FEED_UID, Direction::Output)
            .context("the wsctl virtual devices are missing, run: wsctl audio install")?;

        // What `wsctl audio calibrate` measured for this exact pair, if anyone
        // ever ran it. Nothing here can fail: an unmeasured pair, an unreadable
        // file and a file from another version all mean the same thing, which
        // is that the starting point comes from what Core Audio reports and
        // nothing else. It is only ever a starting point now, so a stale one is
        // said out loud rather than thrown away.
        let measured = Corrections::load().find(output_uid, input_uid).cloned();
        // A clock that will not answer is not grounds for calling anything
        // stale, so an unknown "now" leaves the measurement as it found it.
        let now = correction::now();
        let stale = measured
            .as_ref()
            .zip(now)
            .is_some_and(|(c, now)| c.is_stale(now));
        match &measured {
            Some(c) if stale => tracing::warn!(
                offset_ms = c.offset_ms(),
                age_days = now.and_then(|now| c.age_secs(now)).map(|a| a / 86_400),
                "the calibration for this pair is old, using it only as a starting point"
            ),
            Some(c) => tracing::info!(
                offset_ms = c.offset_ms(),
                measured_ms = c.measured_ms,
                "starting from a measured delay"
            ),
            None => tracing::info!("no calibration for this pair, trusting Core Audio"),
        }

        let shared = Arc::new(Shared::new(
            &tap,
            measured.as_ref().map(|c| c.offset_ms()),
            stale,
        ));
        let endpoints = Arc::new(Mutex::new(vec![
            Endpoint::new(Role::Tap, tap.uid.clone(), Direction::Input),
            Endpoint::new(Role::Feed, feed.uid.clone(), Direction::Output),
            Endpoint::new(Role::Microphone, input_uid.to_string(), Direction::Input),
            Endpoint::new(Role::Speaker, output_uid.to_string(), Direction::Output),
        ]));

        let worker = std::thread::Builder::new()
            .name("wsctl-bridge".into())
            .spawn({
                let shared = shared.clone();
                move || run_canceller(shared)
            })?;

        // Assembled before the devices are touched, so that anything failing
        // from here on tears the whole thing down on its way out.
        let mut engine = Engine {
            shared,
            endpoints,
            worker: Some(worker),
            supervisor: None,
        };

        // The hardware ends are allowed to be missing: a Bluetooth speaker that
        // is not there yet is the same problem as one that drops out later, and
        // this way both go through the one path.
        reconcile(&engine.endpoints, &engine.shared);

        engine.supervisor = Some(
            std::thread::Builder::new()
                .name("wsctl-bridge-devices".into())
                .spawn({
                    let shared = engine.shared.clone();
                    let endpoints = engine.endpoints.clone();
                    move || {
                        while !shared.wait_for_stop(Duration::from_secs(1)) {
                            reconcile(&endpoints, &shared);
                        }
                    }
                })?,
        );
        Ok(engine)
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shared.stop();
        for thread in [self.supervisor.take(), self.worker.take()]
            .into_iter()
            .flatten()
        {
            let _ = thread.join();
        }
        // Last, so no callback is still pushing into a ring the worker owns.
        self.endpoints
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        tracing::info!("bridge stopped");
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Role {
    Tap,
    Speaker,
    Microphone,
    Feed,
}

/// One device the bridge wants attached, and the stream attached to it. The
/// stream is `None` whenever the device is not there.
struct Endpoint {
    role: Role,
    uid: String,
    direction: Direction,
    attached: Option<Attached>,
}

struct Attached {
    id: device::Id,
    /// What this endpoint contributes to the round trip the canceller has to
    /// know about: the device's own latency, plus the ring the bridge keeps in
    /// front of it.
    lag: Lag,
    /// Held only so that dropping the endpoint stops the device.
    _stream: Stream,
}

#[derive(Clone, Copy, Default, PartialEq, Debug)]
struct Lag {
    device_ms: f64,
    buffer_ms: f64,
}

impl Lag {
    fn total_ms(self) -> f64 {
        self.device_ms + self.buffer_ms
    }
}

impl Endpoint {
    fn new(role: Role, uid: String, direction: Direction) -> Self {
        Endpoint {
            role,
            uid,
            direction,
            attached: None,
        }
    }

    fn lag(&self) -> Lag {
        self.attached.as_ref().map(|a| a.lag).unwrap_or_default()
    }
}

/// Attaches every endpoint that is missing, and drops any whose device has
/// gone. Runs once at startup and once a second after that, which is what
/// makes a Bluetooth speaker dropping out a two-line log line instead of a
/// dead bridge.
fn reconcile(endpoints: &Mutex<Vec<Endpoint>>, shared: &Arc<Shared>) {
    let mut endpoints = endpoints.lock().unwrap_or_else(|e| e.into_inner());
    let mut changed = false;

    for endpoint in endpoints.iter_mut() {
        let live = device::find(&endpoint.uid);
        match (live, &endpoint.attached) {
            (Some(id), Some(attached)) if id == attached.id => continue,
            (None, None) => continue,
            (None, Some(_)) => {
                tracing::warn!(uid = endpoint.uid, "device went away");
                endpoint.attached = None;
                changed = true;
            }
            (Some(_), _) => {
                // Dropped first: the old stream has to let go of its end of the
                // ring before the new one can take it.
                endpoint.attached = None;
                match attach(endpoint, shared) {
                    Ok(attached) => {
                        endpoint.attached = Some(attached);
                        changed = true;
                    }
                    Err(e) => tracing::warn!("could not attach {}: {e:#}", endpoint.uid),
                }
            }
        }
    }

    if !changed {
        return;
    }

    let find = |role| endpoints.iter().find(|e| e.role == role);
    let attached = |role| find(role).is_some_and(|e| e.attached.is_some());
    shared
        .speaker_attached
        .store(attached(Role::Speaker), Ordering::Relaxed);
    shared
        .microphone_attached
        .store(attached(Role::Microphone), Ordering::Relaxed);

    // Only worth applying with both ends of the loop present: it was measured
    // for that pair together, and half a pair is not the thing that was
    // measured.
    let correction = match (attached(Role::Speaker), attached(Role::Microphone)) {
        (true, true) => shared.correction_ms,
        _ => 0.0,
    };
    let delay = render_to_capture_ms(
        find(Role::Speaker).map(Endpoint::lag).unwrap_or_default(),
        find(Role::Microphone)
            .map(Endpoint::lag)
            .unwrap_or_default(),
        shared.reference_lag_ms(),
        correction,
    );
    if shared.delay_ms.swap(delay, Ordering::Relaxed) != delay {
        tracing::info!(delay, "render to capture delay");
    }
}

fn attach(endpoint: &Endpoint, shared: &Arc<Shared>) -> Result<Attached> {
    let device = device::open(&endpoint.uid, endpoint.direction)?;
    tracing::info!(
        uid = device.uid,
        name = device.name,
        rate = device.rate,
        channels = device.channels,
        latency_ms = device.latency_ms(),
        "attaching {:?}",
        endpoint.role
    );

    let (stream, buffer_ms) = match endpoint.role {
        Role::Tap => tap_stream(&device, shared),
        Role::Speaker => speaker_stream(&device, shared),
        Role::Microphone => microphone_stream(&device, shared),
        Role::Feed => feed_stream(&device, shared),
    }?;
    Ok(Attached {
        id: device.id,
        lag: Lag {
            device_ms: device.latency_ms(),
            buffer_ms,
        },
        _stream: stream,
    })
}

/// Milliseconds between a frame being handed to the canceller as the reference
/// and its echo coming back in a capture frame. Getting this wrong is the one
/// mistake the canceller cannot recover from, so it is worked out from what the
/// devices report and what the bridge itself is holding, never guessed.
///
/// A frame handed over as the reference still has to wait out the render ring
/// and the speaker before it is audible, and its echo then waits out the
/// microphone and the capture ring before it is handed over in its turn. The
/// reference itself is handed over slightly late, which comes back off.
///
/// `correction_ms` is what `wsctl audio calibrate` found the devices' own
/// reported latencies to be out by, and is zero until somebody measures it. It
/// carries the flight through the air, which Core Audio cannot know about, and
/// whatever the device lied about, which for a Bluetooth speaker is the larger
/// of the two.
fn render_to_capture_ms(
    speaker: Lag,
    microphone: Lag,
    reference_lag_ms: f64,
    correction_ms: f64,
) -> i32 {
    (speaker.total_ms() + microphone.total_ms() - reference_lag_ms + correction_ms).max(0.0) as i32
}

/// Reads what the apps are playing and fans it out twice: once towards the
/// real speaker, once as the mono reference the canceller subtracts.
fn tap_stream(device: &Device, shared: &Arc<Shared>) -> Result<(Stream, f64)> {
    let mut render = shared
        .render
        .producer()
        .context("the tap is already attached")?;
    let mut reference = shared
        .reference
        .producer()
        .context("the tap is already attached")?;

    let channels = device.channels;
    let shared = shared.clone();
    let mut mono = vec![0.0f32; MAX_BLOCK_FRAMES];

    Stream::input(device, move |samples| {
        let frames = samples.len() / channels;
        if frames > mono.len() {
            shared.glitched.fetch_add(frames as u64, Ordering::Relaxed);
            return;
        }
        shared
            .tap_frames
            .fetch_add(frames as u64, Ordering::Relaxed);

        // With no speaker there is nowhere for this to go, and filling the ring
        // with audio nobody will play only counts up glitches. Whatever is left
        // over is thrown away when a speaker does turn up.
        if shared.speaker_attached.load(Ordering::Relaxed) {
            let pushed = render.push(samples);
            if pushed < samples.len() {
                shared
                    .glitched
                    .fetch_add((samples.len() - pushed) as u64, Ordering::Relaxed);
            }
        }

        let scale = 1.0 / channels as f32;
        for (frame, block) in mono[..frames].iter_mut().zip(samples.chunks(channels)) {
            *frame = block.iter().sum::<f32>() * scale;
        }
        reference.push(&mono[..frames]);

        if let Some(worker) = shared.worker.get() {
            worker.unpark();
        }
    })
    .map(|stream| (stream, 0.0))
}

/// Plays what the tap heard, on the speaker's clock rather than the host's.
fn speaker_stream(device: &Device, shared: &Arc<Shared>) -> Result<(Stream, f64)> {
    let mut render = shared
        .render
        .consumer()
        .context("the speaker is already attached")?;
    shared.render.clear();

    let source_channels = shared.tap_channels;
    let channels = device.channels;
    let tap_rate = shared.tap_rate;
    let target = target_frames(
        tap_rate,
        device.block_frames as f64 * tap_rate / device.rate,
    );
    let mut resampler = Resampler::new(
        tap_rate,
        device.rate,
        source_channels,
        target,
        device.block_frames as usize,
    );
    let mut scratch = vec![0.0f32; MAX_BLOCK_FRAMES * source_channels];
    let shared = shared.clone();

    Stream::output(device, move |out| {
        let frames = out.len() / channels;
        if frames > MAX_BLOCK_FRAMES {
            out.fill(0.0);
            shared.glitched.fetch_add(frames as u64, Ordering::Relaxed);
            return;
        }
        let scratch = &mut scratch[..frames * source_channels];
        shared.count(resampler.process(&mut render, scratch));

        for (frame, block) in out
            .chunks_mut(channels)
            .zip(scratch.chunks(source_channels))
        {
            for (channel, sample) in frame.iter_mut().enumerate() {
                *sample = block[channel % source_channels];
            }
        }
        shared
            .speaker_frames
            .fetch_add(frames as u64, Ordering::Relaxed);
        shared
            .speaker_ratio
            .store(resampler.ratio().to_bits(), Ordering::Relaxed);
    })
    .map(|stream| (stream, ms(target, tap_rate)))
}

/// Reads the real microphone. Everything downstream is mono, because a stereo
/// reference is what aec3 0.3.2 cannot do, and matching them keeps the two
/// sides of the canceller honest.
fn microphone_stream(device: &Device, shared: &Arc<Shared>) -> Result<(Stream, f64)> {
    let mut capture = shared
        .capture
        .producer()
        .context("the microphone is already attached")?;
    shared.capture.clear();

    // The worker reads both of these when it rebuilds its resampler, so they
    // are published before the first frame can arrive.
    let target = target_frames(
        device.rate,
        device.block_frames.max(frame_frames(device.rate)) as f64,
    );
    shared.capture_target.store(target, Ordering::Relaxed);
    shared
        .microphone_rate
        .store(device.rate.to_bits(), Ordering::Release);

    let channels = device.channels;
    let mut mono = vec![0.0f32; MAX_BLOCK_FRAMES];
    let shared = shared.clone();

    Stream::input(device, move |samples| {
        let frames = samples.len() / channels;
        if frames > mono.len() {
            shared.glitched.fetch_add(frames as u64, Ordering::Relaxed);
            return;
        }
        let scale = 1.0 / channels as f32;
        for (frame, block) in mono[..frames].iter_mut().zip(samples.chunks(channels)) {
            *frame = block.iter().sum::<f32>() * scale;
        }
        let pushed = capture.push(&mono[..frames]);
        if pushed < frames {
            shared
                .glitched
                .fetch_add((frames - pushed) as u64, Ordering::Relaxed);
        }
        shared
            .microphone_frames
            .fetch_add(frames as u64, Ordering::Relaxed);
    })
    .map(|stream| (stream, ms(target, device.rate)))
}

/// Hands the cleaned microphone back to the apps.
fn feed_stream(device: &Device, shared: &Arc<Shared>) -> Result<(Stream, f64)> {
    let mut clean = shared
        .clean
        .consumer()
        .context("the feed is already attached")?;
    shared.clean.clear();

    let channels = device.channels;
    let mut resampler = Resampler::new(
        shared.tap_rate,
        device.rate,
        1,
        target_frames(
            shared.tap_rate,
            device.block_frames as f64 * shared.tap_rate / device.rate,
        ),
        device.block_frames as usize,
    );
    let mut scratch = vec![0.0f32; MAX_BLOCK_FRAMES];
    let shared = shared.clone();

    Stream::output(device, move |out| {
        let frames = out.len() / channels;
        if frames > scratch.len() {
            out.fill(0.0);
            shared.glitched.fetch_add(frames as u64, Ordering::Relaxed);
            return;
        }
        let scratch = &mut scratch[..frames];
        shared.count(resampler.process(&mut clean, scratch));

        for (frame, sample) in out.chunks_mut(channels).zip(scratch.iter()) {
            frame.fill(*sample);
        }
        shared
            .feed_frames
            .fetch_add(frames as u64, Ordering::Relaxed);
    })
    .map(|stream| (stream, 0.0))
}

/// The one thread that runs the canceller. It is woken by the tap and works in
/// 10 ms frames, pairing each reference frame with the microphone audio that
/// arrived alongside it.
fn run_canceller(shared: Arc<Shared>) {
    let _ = shared.worker.set(std::thread::current());

    let rate = shared.tap_rate as u32;
    let mut canceller = match Canceller::new(rate, rate, shared.delay_ms.load(Ordering::Relaxed)) {
        Ok(canceller) => canceller,
        Err(e) => {
            tracing::error!("could not start the echo canceller: {e:#}");
            return;
        }
    };

    let mut denoiser = Denoiser::new(shared.tap_rate);

    let mut reference = shared.reference.consumer().expect("one worker");
    let mut capture = shared.capture.consumer().expect("one worker");
    let mut clean = shared.clean.producer().expect("one worker");

    let frame = canceller.frame();
    let mut reference_frame = vec![0.0f32; frame];
    let mut capture_frame = vec![0.0f32; frame];
    let mut cleaned = vec![0.0f32; frame];

    let mut microphone: Option<(f64, Resampler)> = None;
    let mut complained = false;

    while !shared.stopping.load(Ordering::Relaxed) {
        let rate = f64::from_bits(shared.microphone_rate.load(Ordering::Acquire));
        if rate > 0.0 && microphone.as_ref().is_none_or(|(known, _)| *known != rate) {
            tracing::info!(rate, "microphone clock changed");
            let target = shared.capture_target.load(Ordering::Relaxed);
            microphone = Some((
                rate,
                Resampler::new(rate, shared.tap_rate, 1, target, frame),
            ));
        }
        let delay_ms = shared.delay_ms.load(Ordering::Relaxed);
        match canceller.set_delay_hint_ms(delay_ms) {
            Ok(true) => tracing::info!(delay_ms, "canceller delay hint moved"),
            Ok(false) => {}
            Err(e) => tracing::warn!("could not move the canceller delay hint: {e:#}"),
        }

        // Two suppressors in series is worse than either alone, so aec3 hands
        // over to RNNoise rather than working alongside it.
        let denoising = denoise();
        denoiser.set_enabled(denoising);
        denoiser.set_threshold(voice_threshold());
        match canceller.set_noise_suppression(!denoising) {
            Ok(true) => tracing::info!(denoising, "noise suppression changed hands"),
            Ok(false) => {}
            Err(e) => tracing::warn!("could not switch aec3 noise suppression: {e:#}"),
        }

        // A backlog here is either the tap's startup burst or a machine that
        // was too busy to run this thread. Either way the audio is stale and
        // playing it out only holds the canceller behind the speaker.
        let backlog = reference.len();
        if backlog > frame * BACKLOG_FRAMES {
            let dropped = reference.skip(backlog - frame);
            shared.resynced.fetch_add(dropped as u64, Ordering::Relaxed);
        }

        while reference.len() >= frame {
            reference.pop(&mut reference_frame);
            match &mut microphone {
                Some((_, resampler)) => {
                    shared.count(resampler.process(&mut capture, &mut capture_frame))
                }
                None => capture_frame.fill(0.0),
            }

            let cancelled = canceller
                .render(&reference_frame)
                .and_then(|()| canceller.capture(&capture_frame, &mut cleaned));
            match cancelled {
                Ok(true) => {
                    // Strictly after the canceller. RNNoise is a nonlinear gain
                    // and aec3 can only subtract an echo it can model linearly,
                    // so in front of it there would be nothing left to cancel.
                    denoiser.process(&mut cleaned);
                    clean.push(&cleaned);
                }
                Ok(false) => {}
                Err(e) => {
                    if !complained {
                        complained = true;
                        tracing::error!("the echo canceller is failing: {e:#}");
                    }
                    break;
                }
            }
        }
        shared
            .erle
            .store(canceller.erle_db().to_bits(), Ordering::Relaxed);
        shared
            .found_delay_ms
            .store(canceller.found_delay_ms(), Ordering::Relaxed);
        shared
            .voice
            .store(denoiser.voice().to_bits(), Ordering::Relaxed);

        // The tap unparks this, so the timeout is only a backstop for the case
        // where the tap itself is not running.
        std::thread::park_timeout(Duration::from_millis(5));
    }
}

/// Everything the threads and the callbacks share. Only atomics and rings, so
/// nothing here can block an audio thread.
struct Shared {
    render: Arc<ring::Shared>,
    reference: Arc<ring::Shared>,
    capture: Arc<ring::Shared>,
    clean: Arc<ring::Shared>,

    tap_rate: f64,
    tap_channels: usize,
    tap_block_frames: usize,

    /// What a calibration run found the reported latencies to be out by, or
    /// zero when nobody has measured this pair.
    correction_ms: f64,
    calibrated: bool,
    /// Whether that measurement is old enough that it should not be quoted as
    /// describing the hardware as it is now.
    stale_calibration: bool,

    microphone_rate: AtomicU64,
    capture_target: AtomicUsize,
    speaker_ratio: AtomicU64,
    delay_ms: AtomicI32,
    found_delay_ms: AtomicI32,
    erle: AtomicU32,
    voice: AtomicU32,

    tap_frames: AtomicU64,
    speaker_frames: AtomicU64,
    microphone_frames: AtomicU64,
    feed_frames: AtomicU64,
    glitched: AtomicU64,
    resynced: AtomicU64,
    speaker_attached: AtomicBool,
    microphone_attached: AtomicBool,

    worker: OnceLock<Thread>,
    stopping: AtomicBool,
    wake: (Mutex<bool>, Condvar),
}

impl Shared {
    fn new(tap: &Device, correction_ms: Option<f64>, stale_calibration: bool) -> Self {
        let capacity = |rate: f64, channels: usize| {
            ring::ring((rate * CAPACITY_MS / 1000.0) as usize * channels)
        };
        Shared {
            render: capacity(tap.rate, tap.channels),
            reference: capacity(tap.rate, 1),
            capture: capacity(96_000.0, 1),
            clean: capacity(tap.rate, 1),

            tap_rate: tap.rate,
            tap_channels: tap.channels,
            tap_block_frames: tap.block_frames as usize,

            correction_ms: correction_ms.unwrap_or(0.0),
            calibrated: correction_ms.is_some(),
            stale_calibration,

            microphone_rate: AtomicU64::new(0),
            capture_target: AtomicUsize::new(0),
            speaker_ratio: AtomicU64::new(1.0f64.to_bits()),
            delay_ms: AtomicI32::new(0),
            found_delay_ms: AtomicI32::new(0),
            erle: AtomicU32::new(0),
            voice: AtomicU32::new(0),

            tap_frames: AtomicU64::new(0),
            speaker_frames: AtomicU64::new(0),
            microphone_frames: AtomicU64::new(0),
            feed_frames: AtomicU64::new(0),
            glitched: AtomicU64::new(0),
            resynced: AtomicU64::new(0),
            speaker_attached: AtomicBool::new(false),
            microphone_attached: AtomicBool::new(false),

            worker: OnceLock::new(),
            stopping: AtomicBool::new(false),
            wake: (Mutex::new(false), Condvar::new()),
        }
    }

    /// Books what a resampler could not deliver. Called from audio threads, so
    /// it is two relaxed adds and nothing else.
    fn count(&self, loss: Loss) {
        if loss.starved > 0 {
            self.glitched
                .fetch_add(loss.starved as u64, Ordering::Relaxed);
        }
        if loss.dropped > 0 {
            self.resynced
                .fetch_add(loss.dropped as u64, Ordering::Relaxed);
        }
    }

    fn stats(&self) -> Stats {
        Stats {
            tap_frames: self.tap_frames.load(Ordering::Relaxed),
            speaker_frames: self.speaker_frames.load(Ordering::Relaxed),
            microphone_frames: self.microphone_frames.load(Ordering::Relaxed),
            feed_frames: self.feed_frames.load(Ordering::Relaxed),
            glitched: self.glitched.load(Ordering::Relaxed)
                + (self.render.lost() + self.capture.lost() + self.clean.lost()) as u64,
            resynced: self.resynced.load(Ordering::Relaxed),
            delay_ms: self.delay_ms.load(Ordering::Relaxed),
            found_delay_ms: self.found_delay_ms.load(Ordering::Relaxed),
            calibrated: self.calibrated,
            stale_calibration: self.stale_calibration,
            correction_ms: self.correction_ms,
            erle_db: f32::from_bits(self.erle.load(Ordering::Relaxed)),
            denoising: denoise(),
            voice: f32::from_bits(self.voice.load(Ordering::Relaxed)),
            speaker_drift_ppm: (f64::from_bits(self.speaker_ratio.load(Ordering::Relaxed)) - 1.0)
                * 1e6,
            speaker_attached: self.speaker_attached.load(Ordering::Relaxed),
            microphone_attached: self.microphone_attached.load(Ordering::Relaxed),
        }
    }

    /// How late the reference reaches the canceller: the tap hands over a whole
    /// block at once, so on average its samples are half a block old, and the
    /// worker is woken by that same push.
    fn reference_lag_ms(&self) -> f64 {
        ms(self.tap_block_frames, self.tap_rate) / 2.0
    }

    fn stop(&self) {
        self.stopping.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.get() {
            worker.unpark();
        }
        let (lock, condvar) = &self.wake;
        *lock.lock().unwrap_or_else(|e| e.into_inner()) = true;
        condvar.notify_all();
    }

    /// Sleeps until told to stop, or for `timeout`. `true` means stop.
    fn wait_for_stop(&self, timeout: Duration) -> bool {
        let (lock, condvar) = &self.wake;
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let (guard, _) = condvar
            .wait_timeout(guard, timeout)
            .unwrap_or_else(|e| e.into_inner());
        *guard
    }
}

/// How full a ring should be kept: enough for two blocks from either end, and
/// never less than [`TARGET_MS`], since a target below one block is a ring
/// that swings between empty and full instead of sitting still.
fn target_frames(rate: f64, block_frames: f64) -> usize {
    ((rate * TARGET_MS / 1000.0).max(block_frames * 2.0)) as usize
}

fn ms(frames: usize, rate: f64) -> f64 {
    frames as f64 * 1000.0 / rate
}

/// Frames in one 10 ms canceller frame at `rate`.
fn frame_frames(rate: f64) -> u32 {
    (rate / 100.0) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    // One test for the whole lifecycle, because the engine is process-wide and
    // splitting this up would only have the halves race each other. It runs
    // against the real devices, and says so and stops when they are not there.
    #[test]
    fn the_lifecycle_holds_together() {
        stop().unwrap();
        stop().unwrap();
        assert!(!is_running());

        // No speaker on purpose: the point here is the engine, and this way the
        // test stays silent whoever runs it.
        let started = start(TAP_UID, "wsctl-test-no-such-speaker");
        if device::find(TAP_UID).is_none() {
            assert!(started.is_err(), "started without the virtual devices");
            return;
        }
        started.unwrap();
        assert!(is_running());

        std::thread::sleep(Duration::from_millis(300));
        let running = stats().expect("running but no stats");
        assert!(running.tap_frames > 0, "the tap never delivered a frame");
        assert!(!running.speaker_attached);

        // Starting again replaces what is there rather than piling up.
        start(TAP_UID, "wsctl-test-no-such-speaker").unwrap();
        assert!(is_running());

        stop().unwrap();
        stop().unwrap();
        assert!(!is_running());
        assert!(stats().is_none());
    }

    // A Bluetooth speaker 200 ms out, a USB microphone 10 ms out, 20 ms of
    // ring in front of each, and a reference handed over 5 ms late.
    #[test]
    fn the_delay_is_the_whole_round_trip() {
        let speaker = Lag {
            device_ms: 200.0,
            buffer_ms: 20.0,
        };
        let microphone = Lag {
            device_ms: 10.0,
            buffer_ms: 20.0,
        };
        assert_eq!(render_to_capture_ms(speaker, microphone, 5.0, 0.0), 245);
    }

    #[test]
    fn a_missing_endpoint_leaves_the_delay_at_zero() {
        assert_eq!(
            render_to_capture_ms(Lag::default(), Lag::default(), 5.0, 0.0),
            0
        );
    }

    // A speaker that claims 94 ms and was measured at 203: the correction
    // carries the whole of the difference, and it goes on top of the rings the
    // bridge is holding rather than replacing them.
    #[test]
    fn a_measured_correction_moves_the_delay_by_exactly_what_was_measured() {
        let speaker = Lag {
            device_ms: 94.0,
            buffer_ms: 20.0,
        };
        let microphone = Lag {
            device_ms: 14.0,
            buffer_ms: 20.0,
        };
        let reported = render_to_capture_ms(speaker, microphone, 5.0, 0.0);
        let measured = render_to_capture_ms(speaker, microphone, 5.0, 203.0 - 108.0);
        assert_eq!(reported, 143);
        assert_eq!(measured - reported, 95);
    }

    // A correction big enough to go negative cannot drive the canceller past
    // the start of its own reference.
    #[test]
    fn a_correction_never_pushes_the_delay_below_nothing() {
        let speaker = Lag {
            device_ms: 30.0,
            buffer_ms: 0.0,
        };
        assert_eq!(
            render_to_capture_ms(speaker, Lag::default(), 0.0, -500.0),
            0
        );
    }

    // The calibrator measures the speaker's reported latency plus the
    // microphone's, so what it hands back has to be comparable with the half of
    // this that the room has anything to say about. Strip the rings and the
    // reference lag and that is exactly what is left.
    #[test]
    fn what_is_calibrated_is_what_the_devices_report() {
        let speaker = Lag {
            device_ms: 94.0,
            buffer_ms: 0.0,
        };
        let microphone = Lag {
            device_ms: 14.0,
            buffer_ms: 0.0,
        };
        assert_eq!(render_to_capture_ms(speaker, microphone, 0.0, 0.0), 108);
    }

    #[test]
    fn a_target_always_covers_two_blocks() {
        // 20 ms at 48 kHz is 960 frames, but two 512 frame blocks is more.
        assert_eq!(target_frames(48_000.0, 512.0), 1024);
        assert_eq!(target_frames(48_000.0, 256.0), 960);
        assert_eq!(target_frames(48_000.0, 2048.0), 4096);
        assert_eq!(target_frames(16_000.0, 160.0), 320);
    }
}
