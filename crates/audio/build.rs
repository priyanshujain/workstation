use std::path::PathBuf;
use std::process::Command;

// Builds the HAL plug-ins and packs them into the binary. They are tarred while
// still signed, and extracted verbatim at install time, so the ad-hoc signature
// stays valid and nothing has to be re-signed on the target machine.
fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let driver = manifest
        .join("../../audio/driver")
        .canonicalize()
        .expect("audio/driver not found");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    for f in [
        "vendor/BlackHole.c",
        "build.sh",
        "install.sh",
        "uninstall.sh",
        "Info.plist.in",
    ] {
        println!("cargo:rerun-if-changed={}", driver.join(f).display());
    }

    let build_dir = out.join("driver-build");
    run(
        Command::new(driver.join("build.sh")).arg(&build_dir),
        "audio/driver/build.sh",
    );

    run(
        Command::new("tar")
            .arg("czf")
            .arg(out.join("drivers.tar.gz"))
            .arg("-C")
            .arg(&build_dir)
            .arg("WSSpeaker.driver")
            .arg("WSMicrophone.driver"),
        "tar",
    );
}

fn run(cmd: &mut Command, what: &str) {
    let status = cmd
        .status()
        .unwrap_or_else(|e| panic!("could not run {what}: {e}"));
    assert!(status.success(), "{what} failed with {status}");
}
