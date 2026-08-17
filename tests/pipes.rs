use std::io::Read;
use std::process::{Command, Stdio};

/// Rust ignores SIGPIPE, so without restoring it every `wsctl ... | head`
/// ends in a panic and a backtrace once the reader goes away.
#[test]
fn closing_a_pipe_early_does_not_panic() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_wsctl"))
        .args(["profiles"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn wsctl");

    // Close the read end before the child writes, the way `head` does once it
    // has taken the lines it wanted. `profiles` prints through println!, which
    // is the path that panics; clap's own --help output does not go through it.
    drop(child.stdout.take());

    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("stderr")
        .read_to_string(&mut stderr)
        .expect("read stderr");
    child.wait().expect("wait");

    assert!(
        !stderr.contains("panicked"),
        "writing to a closed pipe panicked instead of ending quietly:\n{stderr}"
    );
}
