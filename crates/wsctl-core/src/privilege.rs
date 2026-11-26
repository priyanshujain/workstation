use std::io::{BufRead, IsTerminal, Write};
use std::process::Command;

use crate::Result;

pub const UNATTENDED: &str = "WSCTL_UNATTENDED";

pub fn unattended() -> bool {
    std::env::var_os(UNATTENDED).is_some()
}

pub fn sudo() -> Command {
    let mut command = Command::new("sudo");
    command.args(sudo_flag());
    command
}

pub fn sudo_flag() -> Option<&'static str> {
    if unattended() {
        Some("-n")
    } else if askpass_wanted() {
        Some("-A")
    } else {
        None
    }
}

fn askpass_wanted() -> bool {
    std::env::var_os("SUDO_ASKPASS").is_some() && !std::io::stdin().is_terminal()
}

pub fn confirm(yes: bool, question: &str) -> Result<bool> {
    if yes {
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        return Err(anyhow::anyhow!("{question} Pass --yes to confirm without a terminal.").into());
    }
    eprint!("{question} [y/N] ");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    let answer = answer.trim().to_ascii_lowercase();
    Ok(answer == "y" || answer == "yes")
}
