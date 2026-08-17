//! Drive the connected Android test device.
//!
//! Device selection is the awkward part: an emulator is usually running alongside the phone,
//! and arming `adb tcpip` makes that one phone appear on two transports at once. Both make
//! bare adb commands ambiguous, so [`device::resolve`] settles it in one place.

pub mod adb;
pub mod device;
pub mod input;
pub mod scrcpy;

pub use device::{Device, State, Transport, parse_devices, resolve};
pub use scrcpy::Mode;
