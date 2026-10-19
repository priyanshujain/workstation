use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct Process {
    pub pid: i32,
    pub ppid: i32,
    pub elapsed_secs: u64,
    pub rss_bytes: u64,
    pub detached: bool,
    pub name: String,
    pub args: String,
}

pub struct Table {
    pub processes: HashMap<i32, Process>,
}

impl Table {
    pub fn snapshot() -> Self {
        let out = Command::new("ps")
            .args(["-eww", "-o", "pid=,ppid=,etime=,rss=,tty=,args="])
            .output()
            .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
            .unwrap_or_default();
        let self_pid = std::process::id() as i32;
        let processes = out
            .lines()
            .filter_map(parse_line)
            .filter(|process| process.pid != self_pid)
            .map(|process| (process.pid, process))
            .collect();
        Self { processes }
    }

    pub fn ancestors(&self, pid: i32) -> impl Iterator<Item = &Process> {
        let mut next = self.processes.get(&pid).map(|process| process.ppid);
        std::iter::from_fn(move || {
            let process = self.processes.get(&next?)?;
            next = (process.ppid != process.pid).then_some(process.ppid);
            Some(process)
        })
    }

    pub fn descendants(&self, pid: i32) -> Vec<&Process> {
        let mut found = Vec::new();
        let mut frontier = vec![pid];
        while let Some(parent) = frontier.pop() {
            for process in self.processes.values() {
                if process.ppid == parent && process.pid != parent {
                    found.push(process);
                    frontier.push(process.pid);
                }
            }
        }
        found
    }
}

fn parse_line(line: &str) -> Option<Process> {
    let mut fields = line.split_whitespace();
    let pid = fields.next()?.parse().ok()?;
    let ppid = fields.next()?.parse().ok()?;
    let elapsed_secs = parse_etime(fields.next()?)?;
    let rss_bytes = fields.next()?.parse::<u64>().ok()? * 1024;
    let detached = fields.next()?.starts_with('?');
    let args = fields.collect::<Vec<_>>().join(" ");
    let argv0 = executable(&args)?;
    let name = Path::new(argv0)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| argv0.to_string());
    Some(Process {
        pid,
        ppid,
        elapsed_secs,
        rss_bytes,
        detached,
        name,
        args,
    })
}

fn executable(args: &str) -> Option<&str> {
    let first = args.split_whitespace().next()?;
    let spaced = args
        .match_indices(' ')
        .map(|(end, _)| &args[..end])
        .chain(std::iter::once(args))
        .find(|prefix| prefix.starts_with('/') && Path::new(prefix).is_file());
    Some(spaced.unwrap_or(first))
}

fn parse_etime(etime: &str) -> Option<u64> {
    let (days, clock) = match etime.split_once('-') {
        Some((days, clock)) => (days.parse::<u64>().ok()?, clock),
        None => (0, etime),
    };
    let seconds = clock.split(':').try_fold(0u64, |total, part| {
        Some(total * 60 + part.parse::<u64>().ok()?)
    })?;
    Some(days * 86_400 + seconds)
}

pub fn cwd(pid: i32) -> Option<PathBuf> {
    if cfg!(target_os = "linux") {
        return std::fs::read_link(format!("/proc/{pid}/cwd")).ok();
    }
    let out = Command::new("lsof")
        .args(["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|line| line.strip_prefix('n').map(PathBuf::from))
}

pub fn carrying(markers: &[String]) -> Vec<i32> {
    if markers.is_empty() {
        return Vec::new();
    }
    if cfg!(target_os = "linux") {
        return linux_carrying(markers);
    }
    let out = Command::new("ps")
        .args(["-axE", "-ww", "-o", "pid=,command="])
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default();
    out.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let pid = words.next()?.parse().ok()?;
            words.any(|word| sets_any(word, markers)).then_some(pid)
        })
        .collect()
}

fn linux_carrying(markers: &[String]) -> Vec<i32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse::<i32>().ok())
        .filter(|pid| {
            std::fs::read(format!("/proc/{pid}/environ"))
                .map(|environ| {
                    environ
                        .split(|byte| *byte == 0)
                        .any(|entry| sets_any(&String::from_utf8_lossy(entry), markers))
                })
                .unwrap_or(false)
        })
        .collect()
}

fn sets_any(entry: &str, markers: &[String]) -> bool {
    entry
        .split_once('=')
        .is_some_and(|(name, _)| markers.iter().any(|marker| marker == name))
}

pub fn terminate(pid: i32) -> std::io::Result<()> {
    // SAFETY: kill only sends a signal; an invalid pid returns an error.
    match unsafe { libc::kill(pid, libc::SIGTERM) } {
        0 => Ok(()),
        _ => Err(std::io::Error::last_os_error()),
    }
}
