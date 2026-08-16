use anyhow::Result;
use console::style;

pub fn install() -> Result<()> {
    println!(
        "{} Installing the {} audio devices",
        style("→").cyan(),
        style("wsctl").bold()
    );
    println!(
        "    {}",
        style("coreaudiod restarts, so audio drops for a moment").dim()
    );
    println!();

    audio::install()?;
    println!();
    report(&audio::status()?);
    Ok(())
}

pub fn uninstall() -> Result<()> {
    println!(
        "{} Removing the {} audio devices",
        style("→").cyan(),
        style("wsctl").bold()
    );
    println!();

    audio::uninstall()?;
    println!("{} Removed", style("✓").green());
    Ok(())
}

pub fn status() -> Result<()> {
    report(&audio::status()?);
    Ok(())
}

fn report(s: &audio::Status) {
    if !s.is_installed() {
        println!("{} Not installed", style("×").red());
        println!("    {}", style("run: wsctl audio install").dim());
        return;
    }

    println!("{} Installed", style("✓").green());

    // Present on disk but absent from Core Audio means coreaudiod rejected the
    // plug-in, most often because the bundle was edited after it was signed.
    if !s.is_loaded() {
        println!("{} Not loaded by coreaudiod", style("×").red());
        println!(
            "    {}",
            style("log stream --predicate 'eventMessage CONTAINS \"plug-in named\"'").dim()
        );
        return;
    }

    println!("{} Devices live", style("✓").green());
    for d in audio::DEVICES {
        println!("    {}", style(d).dim());
    }
}

pub fn devices() -> Result<()> {
    for (label, direction) in [
        ("Microphones", audio::device::Direction::Input),
        ("Speakers", audio::device::Direction::Output),
    ] {
        println!("{}", style(label).bold());
        for d in audio::device::list(direction)? {
            println!(
                "    {:<28} {}",
                d.name,
                style(format!(
                    "{} · {:.0} Hz · {} ch · {:.0} ms",
                    d.uid,
                    d.rate,
                    d.channels,
                    d.latency_ms()
                ))
                .dim()
            );
        }
        println!();
    }
    Ok(())
}

pub fn bridge(
    input: &str,
    output: &str,
    seconds: Option<u64>,
    denoise: bool,
    voice_threshold: f32,
) -> Result<()> {
    // Set before the bridge starts, so the first frame is already treated the
    // way it was asked for.
    audio::bridge::set_denoise(denoise);
    audio::bridge::set_voice_threshold(voice_threshold);
    audio::bridge::start(input, output)?;
    println!(
        "{} Bridge running, {}",
        style("→").cyan(),
        style("^C to stop").dim()
    );
    if let Some(s) = audio::bridge::stats() {
        // Only where the search starts. The canceller finds the echo itself and
        // reports where it landed, so this says "starting at" rather than
        // claiming to be the delay that is doing the work.
        let line = match (s.calibrated, s.stale_calibration) {
            (true, false) => format!(
                "starting at {} ms, measured, {:+.0} ms on what macOS reports",
                s.delay_ms, s.correction_ms
            ),
            (true, true) => format!(
                "starting at {} ms, from a measurement over a month old · wsctl audio calibrate -i {input} -o {output}",
                s.delay_ms
            ),
            (false, _) => format!(
                "starting at {} ms, from what macOS reports, never measured · wsctl audio calibrate -i {input} -o {output}",
                s.delay_ms
            ),
        };
        println!("    {}", style(line).dim());
    }

    let started = std::time::Instant::now();
    while seconds.is_none_or(|limit| started.elapsed().as_secs() < limit) {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let Some(s) = audio::bridge::stats() else {
            break;
        };
        println!(
            "    tap {} → speaker {} · mic {} → feed {} · aligned {} ms · erle {:.1} dB{} · drift {:.0} ppm · glitched {} · resynced {}{}",
            s.tap_frames,
            s.speaker_frames,
            s.microphone_frames,
            s.feed_frames,
            // What the canceller found, not what it was told: the two differ
            // by design and only the first one is doing anything.
            s.found_delay_ms,
            s.erle_db,
            if s.denoising {
                format!(" · voice {:.2}", s.voice)
            } else {
                String::new()
            },
            s.speaker_drift_ppm,
            s.glitched,
            s.resynced,
            match (s.speaker_attached, s.microphone_attached) {
                (true, true) => String::new(),
                (false, true) => " · no speaker".to_string(),
                (true, false) => " · no microphone".to_string(),
                (false, false) => " · nothing attached".to_string(),
            }
        );
    }
    audio::bridge::stop()
}

pub fn calibrate(input: &str, output: &str, probes: Option<usize>, dry_run: bool) -> Result<()> {
    let probes = probes.unwrap_or_else(audio::calibrate::default_probes);
    println!(
        "{} Measuring the round trip out of {} and back into {}",
        style("→").cyan(),
        style(output).bold(),
        style(input).bold()
    );
    println!(
        "    {}",
        style(format!(
            "{probes} sweeps, 200 Hz to 10 kHz, played out loud after a few warm-up ones. \
             Keep the room quiet"
        ))
        .dim()
    );
    println!();

    let measured = audio::calibrate::run(input, output, probes)?;
    for (i, reading) in measured.readings.iter().enumerate() {
        let line = format!(
            "    probe {}   {:>7.1} ms   peak x{:.1}",
            i + 1,
            reading.delay_ms,
            reading.confidence
        );
        match reading.is_clear() {
            true => println!("{}", style(line).dim()),
            // Not an error on its own: a run only needs most of them to land.
            false => println!("{}", style(format!("{line}  (too weak, ignored)")).yellow()),
        }
    }
    println!();

    println!(
        "{} {} to {}: {} round trip",
        style("✓").green(),
        style(&measured.output_name).bold(),
        style(&measured.input_name).bold(),
        style(format!("{:.1} ms", measured.measured_ms)).bold()
    );
    let row = |label: &str, value: String| println!("    {label:<22} {value}");
    row("macOS reports", format!("{:.1} ms", measured.computed_ms));
    row(
        "out by",
        format!(
            "{:+.1} ms  ({:+.0}%)",
            measured.difference_ms(),
            measured.difference_ms() / measured.computed_ms.max(1e-9) * 100.0
        ),
    );
    // Said out loud, because a difference of a couple of hundred milliseconds
    // reads like an absurd distance until you notice it cannot be a distance
    // at all. Sound does 34 cm in a millisecond, so anything past a few ms is
    // a device holding audio and not saying so.
    if measured.is_beyond_air() {
        row(
            "if that were air",
            format!(
                "{:.0} m between them, so it is not: it is buffering inside a device",
                measured.difference_as_air_metres().abs()
            ),
        );
    }
    row(
        "probes agree within",
        format!("{:.1} ms", measured.spread_ms),
    );
    row(
        "weakest peak",
        format!("x{:.1} over the next highest", measured.confidence),
    );
    row(
        "rates",
        format!(
            "{:.0} Hz out, {:.0} Hz in",
            measured.output_rate, measured.input_rate
        ),
    );
    println!(
        "    {}",
        style("this is speaker latency + flight through the air + microphone latency, which is what aec3 wants").dim()
    );
    println!();

    if dry_run {
        println!("{} Not saved, --dry-run", style("·").dim());
        return Ok(());
    }

    let mut corrections = audio::correction::Corrections::load();
    corrections.record(measured.correction());
    corrections.save()?;
    println!(
        "{} Saved, the bridge will use it for this pair",
        style("✓").green()
    );
    println!(
        "    {}",
        style(audio::correction::path()?.display().to_string()).dim()
    );
    Ok(())
}
