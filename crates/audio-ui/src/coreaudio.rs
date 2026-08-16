//! Reads the device list out of the HAL.
//!
//! A device carries channels in one direction only as far as this is concerned:
//! a stream configuration with no input channels is not a microphone, whatever
//! else the device may also do.

use std::ffi::c_void;
use std::mem::{offset_of, size_of};
use std::ptr::{self, NonNull};

use anyhow::{Result, bail};
use objc2_core_audio::{
    AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectID,
    AudioObjectPropertyAddress, kAudioDevicePropertyDeviceUID,
    kAudioDevicePropertyStreamConfiguration, kAudioHardwarePropertyDevices,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal,
    kAudioObjectPropertyScopeInput, kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject,
};
use objc2_core_audio_types::{AudioBuffer, AudioBufferList};
use objc2_core_foundation::{CFRetained, CFString};

use crate::bridge::Device;

/// Our own virtual devices. They are the far end of the bridge, never something
/// the user picks as hardware, so they are kept out of both lists.
const OURS: [&str; 4] = [
    "Workstation Speaker",
    "Workstation Speaker Tap",
    "Workstation Mic",
    "Workstation Mic Feed",
];

#[derive(Clone, Copy)]
pub enum Direction {
    Input,
    Output,
}

impl Direction {
    fn scope(self) -> u32 {
        match self {
            Direction::Input => kAudioObjectPropertyScopeInput,
            Direction::Output => kAudioObjectPropertyScopeOutput,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Direction::Input => "input",
            Direction::Output => "output",
        }
    }
}

pub fn devices(direction: Direction) -> Result<Vec<Device>> {
    let mut found = Vec::new();
    for id in device_ids()? {
        if channel_count(id, direction.scope()) == 0 {
            continue;
        }
        let (Some(uid), Some(name)) = (
            string(id, kAudioDevicePropertyDeviceUID),
            string(id, kAudioObjectPropertyName),
        ) else {
            continue;
        };
        if OURS.contains(&name.as_str()) {
            continue;
        }
        tracing::debug!(kind = direction.label(), uid, name, "device");
        found.push(Device { uid, name });
    }
    Ok(found)
}

fn device_ids() -> Result<Vec<AudioObjectID>> {
    let mut addr = address(
        kAudioHardwarePropertyDevices,
        kAudioObjectPropertyScopeGlobal,
    );
    let system = kAudioObjectSystemObject as AudioObjectID;
    let mut size = size_of_property(system, &mut addr)?;

    let mut ids = vec![0 as AudioObjectID; size as usize / size_of::<AudioObjectID>()];
    if ids.is_empty() {
        return Ok(ids);
    }
    let status = unsafe {
        AudioObjectGetPropertyData(
            system,
            NonNull::from(&mut addr),
            0,
            ptr::null(),
            NonNull::from(&mut size),
            NonNull::new(ids.as_mut_ptr().cast::<c_void>()).unwrap(),
        )
    };
    if status != 0 {
        bail!("could not read the device list: Core Audio returned {status}");
    }
    ids.truncate(size as usize / size_of::<AudioObjectID>());
    Ok(ids)
}

/// Sum of the channels on every stream the device has in one scope.
fn channel_count(id: AudioObjectID, scope: u32) -> u32 {
    let mut addr = address(kAudioDevicePropertyStreamConfiguration, scope);
    let Ok(mut size) = size_of_property(id, &mut addr) else {
        return 0;
    };

    // The list is variable length, so it is read into 8-byte words to land on
    // the alignment AudioBufferList needs.
    let mut words = vec![0u64; size.div_ceil(8) as usize];
    if words.is_empty() {
        return 0;
    }
    let status = unsafe {
        AudioObjectGetPropertyData(
            id,
            NonNull::from(&mut addr),
            0,
            ptr::null(),
            NonNull::from(&mut size),
            NonNull::new(words.as_mut_ptr().cast::<c_void>()).unwrap(),
        )
    };
    if status != 0 {
        return 0;
    }

    let list = words.as_ptr().cast::<AudioBufferList>();
    let room = (size as usize).saturating_sub(offset_of!(AudioBufferList, mBuffers))
        / size_of::<AudioBuffer>();
    let count = unsafe { (*list).mNumberBuffers } as usize;
    let buffers = unsafe {
        std::slice::from_raw_parts(
            ptr::addr_of!((*list).mBuffers).cast::<AudioBuffer>(),
            count.min(room),
        )
    };
    buffers.iter().map(|b| b.mNumberChannels).sum()
}

fn string(id: AudioObjectID, selector: u32) -> Option<String> {
    let mut addr = address(selector, kAudioObjectPropertyScopeGlobal);
    let mut value: *const CFString = ptr::null();
    let mut size = size_of::<*const CFString>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            id,
            NonNull::from(&mut addr),
            0,
            ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut value).cast::<c_void>(),
        )
    };
    if status != 0 {
        return None;
    }
    let value = NonNull::new(value.cast_mut())?;
    Some(unsafe { CFRetained::from_raw(value) }.to_string())
}

fn size_of_property(id: AudioObjectID, addr: &mut AudioObjectPropertyAddress) -> Result<u32> {
    let mut size = 0u32;
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            id,
            NonNull::from(addr),
            0,
            ptr::null(),
            NonNull::from(&mut size),
        )
    };
    if status != 0 {
        bail!("Core Audio returned {status}");
    }
    Ok(size)
}

fn address(selector: u32, scope: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // These read whatever this machine has plugged in, so they assert only what
    // has to hold of any device list, including an empty one.
    fn both() -> Vec<Device> {
        let mut all = devices(Direction::Input).unwrap();
        all.extend(devices(Direction::Output).unwrap());
        all
    }

    #[test]
    fn our_own_devices_are_never_offered() {
        for device in both() {
            assert!(
                !OURS.contains(&device.name.as_str()),
                "{} is one of ours",
                device.name
            );
        }
    }

    #[test]
    fn every_device_has_a_uid_and_a_name() {
        for device in both() {
            assert!(!device.uid.is_empty(), "{} has no uid", device.name);
            assert!(!device.name.is_empty(), "{} has no name", device.uid);
        }
    }

    #[test]
    fn a_device_is_listed_once_per_direction() {
        for direction in [Direction::Input, Direction::Output] {
            let mut uids: Vec<String> = devices(direction)
                .unwrap()
                .into_iter()
                .map(|d| d.uid)
                .collect();
            let listed = uids.len();
            uids.sort();
            uids.dedup();
            assert_eq!(uids.len(), listed, "{} has a duplicate", direction.label());
        }
    }
}
