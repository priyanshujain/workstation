use std::ffi::OsStr;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitCode, Stdio};

use wsctl_core::privilege::{UNATTENDED, unattended};

const PINENTRIES: [&str; 4] = ["pinentry-mac", "pinentry-qt", "pinentry-gnome3", "pinentry"];
const MAX_LINE: usize = 1024;
const PART_LIMIT: usize = 400;

fn main() -> ExitCode {
    match ask() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("wsctl-askpass: {message}");
            ExitCode::FAILURE
        }
    }
}

fn ask() -> Result<(), String> {
    if unattended() {
        return Err(format!(
            "{UNATTENDED} is set, so no password dialog is shown"
        ));
    }
    let sudo = std::os::unix::process::parent_id();
    if !runs_as_sudo(sudo) {
        return Err("only sudo may ask for a password through wsctl-askpass".into());
    }
    if !graphical_session() {
        return Err(
            "no graphical session (WAYLAND_DISPLAY or DISPLAY) to show a password dialog in".into(),
        );
    }
    let command =
        process_field(sudo, "args").ok_or("could not read the command sudo wants to run")?;
    let requester = process_field(sudo, "ppid")
        .and_then(|pid| pid.parse().ok())
        .and_then(|pid| process_field(pid, "args"));
    let pinentry = find_pinentry().ok_or(
        "no pinentry found: set WSCTL_PINENTRY or put pinentry-mac, pinentry-qt, pinentry-gnome3 or pinentry on PATH",
    )?;
    let description = describe(&command, requester.as_deref());
    let mut pin = read_pin(&pinentry, &description)?;
    let written = write_pin(&pin);
    wipe(&mut pin);
    written.map_err(|e| format!("could not hand the password to sudo: {e}"))
}

#[cfg(target_os = "macos")]
fn graphical_session() -> bool {
    true
}

#[cfg(not(target_os = "macos"))]
fn graphical_session() -> bool {
    ["WAYLAND_DISPLAY", "DISPLAY"]
        .iter()
        .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
}

fn runs_as_sudo(pid: u32) -> bool {
    let as_root = process_field(pid, "uid").is_some_and(|uid| uid == "0");
    let named_sudo = process_field(pid, "comm")
        .is_some_and(|comm| Path::new(&comm).file_name() == Some(OsStr::new("sudo")));
    as_root && named_sudo
}

fn process_field(pid: u32, field: &str) -> Option<String> {
    let out = Command::new("ps")
        .args(["-o", &format!("{field}="), "-p", &pid.to_string()])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !value.is_empty()).then_some(value)
}

fn describe(command: &str, requester: Option<&str>) -> String {
    let mut description = format!(
        "wsctl needs your password to run as root:%0A{}",
        escape_capped(command, PART_LIMIT)
    );
    if let Some(requester) = requester {
        description.push_str("%0Arequested by: ");
        description.push_str(&escape_capped(requester, PART_LIMIT));
    }
    description
}

fn find_pinentry() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("WSCTL_PINENTRY").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(explicit));
    }
    let path = std::env::var_os("PATH")?;
    PINENTRIES.iter().find_map(|name| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    })
}

fn read_pin(pinentry: &PathBuf, description: &str) -> Result<Vec<u8>, String> {
    let mut child = Command::new(pinentry)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", pinentry.display()))?;
    let result = converse(&mut child, description);
    let _ = child.wait();
    result
}

fn converse(child: &mut Child, description: &str) -> Result<Vec<u8>, String> {
    let mut session = Session {
        input: child.stdin.take().ok_or("pinentry has no stdin")?,
        output: child.stdout.take().ok_or("pinentry has no stdout")?,
    };
    session.expect_ok()?;
    session.request("SETTITLE wsctl")?;
    session.request(&format!("SETDESC {description}"))?;
    session.request("SETPROMPT Password:")?;
    let pin = session.get_pin();
    let _ = session.send("BYE");
    pin
}

struct Session {
    input: ChildStdin,
    output: ChildStdout,
}

impl Session {
    fn send(&mut self, line: &str) -> Result<(), String> {
        writeln!(self.input, "{line}")
            .and_then(|()| self.input.flush())
            .map_err(|e| format!("pinentry stopped listening: {e}"))
    }

    fn request(&mut self, line: &str) -> Result<(), String> {
        self.send(line)?;
        self.expect_ok()
    }

    fn expect_ok(&mut self) -> Result<(), String> {
        let mut data = Vec::new();
        let outcome = self.reply(&mut data);
        wipe(&mut data);
        outcome
    }

    fn get_pin(&mut self) -> Result<Vec<u8>, String> {
        self.send("GETPIN")?;
        let mut pin = Vec::with_capacity(MAX_LINE);
        match self.reply(&mut pin) {
            Ok(()) => Ok(pin),
            Err(e) => {
                wipe(&mut pin);
                Err(e)
            }
        }
    }

    fn reply(&mut self, data: &mut Vec<u8>) -> Result<(), String> {
        let mut line = Vec::with_capacity(MAX_LINE);
        let outcome = loop {
            line.clear();
            if let Err(e) = self.read_line(&mut line) {
                break Err(e);
            }
            if line == b"OK" || line.starts_with(b"OK ") {
                break Ok(());
            }
            if line.starts_with(b"ERR") {
                break Err(format!(
                    "pinentry refused: {}",
                    String::from_utf8_lossy(&line)
                ));
            }
            if let Some(escaped) = line.strip_prefix(b"D ") {
                unescape_into(escaped, data);
            }
        };
        wipe(&mut line);
        outcome
    }

    fn read_line(&mut self, line: &mut Vec<u8>) -> Result<(), String> {
        let mut byte = [0u8; 1];
        loop {
            match self.output.read(&mut byte) {
                Ok(0) => return Err("pinentry closed the connection".into()),
                Ok(_) if byte[0] == b'\n' => return Ok(()),
                Ok(_) if line.len() < MAX_LINE => line.push(byte[0]),
                Ok(_) => return Err("pinentry sent a line that is too long".into()),
                Err(e) => return Err(format!("could not read from pinentry: {e}")),
            }
        }
    }
}

fn escape_capped(text: &str, limit: usize) -> String {
    let full: String = text.chars().map(escape).collect();
    if full.len() <= limit {
        return full;
    }
    let mut capped = String::with_capacity(limit);
    for unit in text.chars().map(escape) {
        if capped.len() + unit.len() + "...".len() > limit {
            break;
        }
        capped.push_str(&unit);
    }
    capped.push_str("...");
    capped
}

fn escape(c: char) -> String {
    match c {
        c if c == '%' || (c as u32) < 0x20 => format!("%{:02X}", c as u32),
        c => c.to_string(),
    }
}

fn unescape_into(escaped: &[u8], out: &mut Vec<u8>) {
    let mut i = 0;
    while i < escaped.len() {
        let decoded = (escaped[i] == b'%')
            .then(|| escaped.get(i + 1..i + 3))
            .flatten()
            .and_then(|hex| u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok());
        match decoded {
            Some(byte) => {
                out.push(byte);
                i += 3;
            }
            None => {
                out.push(escaped[i]);
                i += 1;
            }
        }
    }
}

fn write_pin(pin: &[u8]) -> std::io::Result<()> {
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(pin)?;
    stdout.write_all(b"\n")?;
    stdout.flush()
}

fn wipe(buffer: &mut Vec<u8>) {
    buffer.fill(0);
    std::hint::black_box(&buffer);
    buffer.clear();
}
