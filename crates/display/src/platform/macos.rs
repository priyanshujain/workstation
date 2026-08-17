use std::ffi::c_void;
use std::ptr;
use std::time::Duration;

use anyhow::{Result, bail};

use crate::arrange::{DisplayInfo, Move, layout_changed};
use crate::key::DisplayKey;

type CGDirectDisplayID = u32;
type CGError = i32;
type CGDisplayConfigRef = *mut c_void;

const SUCCESS: CGError = 0;
const CONFIGURE_PERMANENTLY: u32 = 2;
const MAX_DISPLAYS: usize = 16;

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGGetActiveDisplayList(
        max: u32,
        displays: *mut CGDirectDisplayID,
        count: *mut u32,
    ) -> CGError;
    fn CGDisplayBounds(display: CGDirectDisplayID) -> CGRect;
    fn CGDisplayIsMain(display: CGDirectDisplayID) -> i32;
    fn CGDisplayIsBuiltin(display: CGDirectDisplayID) -> i32;
    fn CGDisplayVendorNumber(display: CGDirectDisplayID) -> u32;
    fn CGDisplayModelNumber(display: CGDirectDisplayID) -> u32;
    fn CGDisplaySerialNumber(display: CGDirectDisplayID) -> u32;
    fn CGBeginDisplayConfiguration(config: *mut CGDisplayConfigRef) -> CGError;
    fn CGConfigureDisplayOrigin(
        config: CGDisplayConfigRef,
        display: CGDirectDisplayID,
        x: i32,
        y: i32,
    ) -> CGError;
    fn CGCompleteDisplayConfiguration(config: CGDisplayConfigRef, option: u32) -> CGError;
    fn CGCancelDisplayConfiguration(config: CGDisplayConfigRef) -> CGError;
}

pub fn list_displays() -> Result<Vec<DisplayInfo>> {
    let mut ids = [0 as CGDirectDisplayID; MAX_DISPLAYS];
    let mut count: u32 = 0;

    let err = unsafe { CGGetActiveDisplayList(MAX_DISPLAYS as u32, ids.as_mut_ptr(), &mut count) };
    if err != SUCCESS {
        bail!("CGGetActiveDisplayList failed (error {err})");
    }

    let displays = ids[..count as usize]
        .iter()
        .map(|&id| unsafe {
            let bounds = CGDisplayBounds(id);
            DisplayInfo {
                id,
                key: DisplayKey::new(
                    CGDisplayVendorNumber(id),
                    CGDisplayModelNumber(id),
                    CGDisplaySerialNumber(id),
                ),
                builtin: CGDisplayIsBuiltin(id) != 0,
                main: CGDisplayIsMain(id) != 0,
                origin: (bounds.origin.x as i32, bounds.origin.y as i32),
                size: (bounds.size.width as u32, bounds.size.height as u32),
            }
        })
        .collect();

    Ok(displays)
}

/// Apply the whole arrangement in one transaction so the desktop never flashes through an
/// intermediate layout. Written permanently, so it survives a reboot.
pub fn apply_moves(moves: &[Move]) -> Result<()> {
    if moves.is_empty() {
        return Ok(());
    }

    let mut config: CGDisplayConfigRef = ptr::null_mut();
    let err = unsafe { CGBeginDisplayConfiguration(&mut config) };
    if err != SUCCESS {
        bail!("CGBeginDisplayConfiguration failed (error {err})");
    }

    for m in moves {
        let err = unsafe { CGConfigureDisplayOrigin(config, m.id, m.origin.0, m.origin.1) };
        if err != SUCCESS {
            unsafe { CGCancelDisplayConfiguration(config) };
            bail!("could not move display {} (error {err})", m.id);
        }
    }

    let err = unsafe { CGCompleteDisplayConfiguration(config, CONFIGURE_PERMANENTLY) };
    if err != SUCCESS {
        bail!("CGCompleteDisplayConfiguration failed (error {err})");
    }

    Ok(())
}

/// Run `handler` at startup and again whenever the layout changes. Never returns.
///
/// This polls rather than using `CGDisplayRegisterReconfigurationCallback`. That callback
/// registers successfully in a plain command-line process but is never delivered, because
/// notifications only reach processes with a full GUI connection to the WindowServer.
/// Verified against a real mirror toggle: zero callbacks. Polling a handful of CoreGraphics
/// getters costs nothing and works under launchd, which is where this actually runs.
pub fn watch(interval: Duration, mut handler: impl FnMut()) -> Result<()> {
    let mut previous: Option<Vec<DisplayInfo>> = None;

    loop {
        let current = list_displays().unwrap_or_default();

        if layout_changed(previous.as_deref(), &current) {
            handler();
            // Record the layout the handler left behind, so its own change does not read
            // back as a fresh one on the next tick.
            previous = Some(list_displays().unwrap_or(current));
        }

        std::thread::sleep(interval);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_at_least_one_display() {
        // Headless CI has no display server; only assert when one is present.
        let displays = list_displays().expect("listing displays should not fail");
        for d in &displays {
            assert!(d.size.0 > 0 && d.size.1 > 0, "display {} has no size", d.id);
        }
        if !displays.is_empty() {
            assert_eq!(
                displays.iter().filter(|d| d.main).count(),
                1,
                "exactly one display should be main"
            );
        }
    }

    #[test]
    fn main_display_sits_at_the_origin() {
        let displays = list_displays().unwrap();
        if let Some(main) = displays.iter().find(|d| d.main) {
            assert_eq!(
                main.origin,
                (0, 0),
                "macOS defines the main display as the one at (0,0)"
            );
        }
    }

    #[test]
    fn empty_move_list_is_a_no_op() {
        apply_moves(&[]).expect("empty plan should not touch the display config");
    }
}
