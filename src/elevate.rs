use std::ffi::OsString;
use std::io::IsTerminal;
use std::os::unix::process::CommandExt;
use std::process::Command;

const KEPT: [&str; 10] = [
    "HOME",
    "TMPDIR",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "HOMEBREW_PREFIX",
    "TERM",
    "TERM_PROGRAM",
    "COLORTERM",
    "LANG",
];

pub fn rerun_as_root() {
    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    if is_root() || !interactive {
        return;
    }
    let sudo = |args: &[&str]| {
        Command::new("sudo")
            .args(args)
            .status()
            .is_ok_and(|status| status.success())
    };
    let authorised = sudo(&[
        "-p",
        "Password to also measure root-owned folders: ",
        "true",
    ]) && sudo(&["-n", "true"]);
    if !authorised {
        eprintln!("Continuing without root: root-owned folders stay unmeasured.");
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let error = Command::new("sudo")
        .args(["-n", "--", "/usr/bin/env"])
        .args(assignments(|name| std::env::var_os(name)))
        .arg(exe)
        .args(std::env::args_os().skip(1))
        .exec();
    eprintln!("Continuing without root: {error}");
}

fn assignments(var: impl Fn(&str) -> Option<OsString>) -> Vec<OsString> {
    let mut kept: Vec<(&str, OsString)> = KEPT
        .iter()
        .filter_map(|name| Some((*name, var(name)?)))
        .collect();
    if let Some(path) = var("PATH") {
        kept.push((disk::owners::USER_PATH, path));
    }
    kept.into_iter()
        .map(|(name, value)| {
            let mut assignment = OsString::from(name);
            assignment.push("=");
            assignment.push(value);
            assignment
        })
        .collect()
}

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_users_home_and_path_reach_the_root_run_and_nothing_else_does() {
        let env = |name: &str| match name {
            "HOME" => Some(OsString::from("/home/u")),
            "PATH" => Some(OsString::from("/home/u/bin:/usr/bin")),
            "SSH_AUTH_SOCK" => Some(OsString::from("/tmp/agent")),
            _ => None,
        };

        assert_eq!(
            assignments(env),
            vec![
                OsString::from("HOME=/home/u"),
                OsString::from(format!("{}=/home/u/bin:/usr/bin", disk::owners::USER_PATH)),
            ]
        );
    }
}
