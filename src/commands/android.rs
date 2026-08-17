use std::process::Command;

use android::{Mode, adb, input, resolve, scrcpy};
use anyhow::{Context, Result, bail};
use console::style;

pub fn list() -> Result<()> {
    let devices = adb::devices()?;

    println!();
    println!("  {}", style("Android Devices").bold().underlined().cyan());
    println!();

    if devices.is_empty() {
        println!("  {}", style("Nothing connected.").dim());
        println!();
        return Ok(());
    }

    let target = resolve(&devices, None).ok().map(|d| d.serial.clone());

    for device in &devices {
        let mut tags = Vec::new();
        if Some(&device.serial) == target.as_ref() {
            tags.push(style("target").green().bold().to_string());
        }
        if device.is_emulator() {
            tags.push(style("emulator").dim().to_string());
        }
        if !device.is_ready() {
            tags.push(style(device.state.to_string()).red().to_string());
        }
        let tags = if tags.is_empty() {
            String::new()
        } else {
            format!("  [{}]", tags.join(" "))
        };

        let name = device.model.as_deref().unwrap_or("unknown");
        println!("  {}{}", style(name).white().bold(), tags);
        println!(
            "    {}  {}",
            style("serial   ").dim(),
            style(&device.serial).yellow()
        );
        println!(
            "    {}  {}",
            style("transport").dim(),
            style(device.transport.to_string()).dim()
        );
    }

    if target.is_none() {
        println!();
        println!("  {}", style("No device would be selected.").dim());
    }
    println!();
    Ok(())
}

pub fn paste(device: Option<String>, text: Option<String>) -> Result<()> {
    let target = adb::target(device.as_deref())?;

    let raw = match text {
        Some(given) => given,
        None => mac_clipboard()?,
    };
    // A trailing newline is an artefact of copying a whole line, and typing it would submit
    // the field. Newlines in the middle mean this is not something you want typed at all.
    let text = raw.trim_end_matches(['\n', '\r']);
    if text.contains('\n') {
        bail!("this is multi-line, use `wsctl android mirror` and Cmd+V instead");
    }

    input::type_text(&target.serial, text)?;

    let name = target.model.as_deref().unwrap_or(&target.serial);
    println!();
    println!(
        "  {} {} into {}",
        style("Typed").green().bold(),
        style(preview(text)).white(),
        style(name).white().bold()
    );
    println!();
    Ok(())
}

fn mac_clipboard() -> Result<String> {
    let output = Command::new("pbpaste")
        .output()
        .context("failed to read the Mac clipboard")?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn preview(text: &str) -> String {
    const MAX: usize = 48;
    if text.chars().count() <= MAX {
        return format!("{text:?}");
    }
    let head: String = text.chars().take(MAX).collect();
    format!("{head:?}...")
}

pub fn clip(device: Option<String>) -> Result<()> {
    start(Mode::Clip, device)
}

pub fn mirror(device: Option<String>) -> Result<()> {
    start(Mode::Mirror, device)
}

fn start(mode: Mode, device: Option<String>) -> Result<()> {
    let target = adb::target(device.as_deref())?;
    let name = target.model.as_deref().unwrap_or(&target.serial);

    let (what, hint) = match mode {
        Mode::Clip => (
            "clipboard bridge",
            "Copy on the phone and it lands on the Mac.",
        ),
        Mode::Mirror => (
            "mirror",
            "Cmd+V pastes the Mac clipboard into the phone, and copying still syncs back.",
        ),
    };

    println!();
    println!(
        "  {} {} on {} {}",
        style("Starting").green().bold(),
        what,
        style(name).white().bold(),
        style(format!("({})", target.transport)).dim()
    );
    println!("  {}", style(hint).dim());
    println!("  {}", style("Ctrl-C to stop.").dim());
    println!();

    scrcpy::run(mode, &target.serial)
}
