//! Core Audio devices, addressed only by UID.
//!
//! Names are neither unique nor stable, so nothing here looks a device up by
//! one. A device is opened by UID, which also means an unplugged device is an
//! absence rather than a stale ID pointing at whatever took its place.

use std::ffi::c_void;
use std::mem::{offset_of, size_of};
use std::ptr::{self, NonNull};

use anyhow::{Result, bail};
use objc2_core_audio::{
    AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart,
    AudioDeviceStop, AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectID,
    AudioObjectPropertyAddress, kAudioDevicePropertyBufferFrameSize, kAudioDevicePropertyDeviceUID,
    kAudioDevicePropertyLatency, kAudioDevicePropertyNominalSampleRate,
    kAudioDevicePropertySafetyOffset, kAudioDevicePropertyStreamConfiguration,
    kAudioDevicePropertyStreams, kAudioHardwarePropertyDevices,
    kAudioHardwarePropertyTranslateUIDToDevice, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeInput,
    kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject, kAudioStreamPropertyLatency,
    kAudioStreamPropertyVirtualFormat,
};
use objc2_core_audio_types::{
    AudioBuffer, AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp,
    kAudioFormatFlagIsFloat, kAudioFormatLinearPCM,
};
use objc2_core_foundation::{CFRetained, CFString};

pub use objc2_core_audio::AudioObjectID as Id;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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
}

/// A device that is open and ready to carry a stream.
pub struct Device {
    pub id: AudioObjectID,
    pub uid: String,
    pub name: String,
    pub direction: Direction,
    pub rate: f64,
    pub channels: usize,
    /// Everything between the IO proc and the outside world, in frames: the
    /// device's own latency, the safety offset, the stream, and the block the
    /// HAL is already holding.
    pub latency_frames: u32,
    pub block_frames: u32,
}

impl Device {
    pub fn latency_ms(&self) -> f64 {
        self.latency_frames as f64 * 1000.0 / self.rate
    }
}

/// Looks a device up by UID. `None` means no device has that UID right now,
/// which for a Bluetooth speaker is a normal Tuesday.
pub fn find(uid: &str) -> Option<AudioObjectID> {
    let uid = CFString::from_str(uid);
    let qualifier: *const CFString = &*uid;
    let mut addr = address(
        kAudioHardwarePropertyTranslateUIDToDevice,
        kAudioObjectPropertyScopeGlobal,
    );
    let mut id: AudioObjectID = 0;
    let mut size = size_of::<AudioObjectID>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            kAudioObjectSystemObject as AudioObjectID,
            NonNull::from(&mut addr),
            size_of::<*const CFString>() as u32,
            (&raw const qualifier).cast::<c_void>(),
            NonNull::from(&mut size),
            NonNull::from(&mut id).cast::<c_void>(),
        )
    };
    (status == 0 && id != 0).then_some(id)
}

pub fn open(uid: &str, direction: Direction) -> Result<Device> {
    let Some(id) = find(uid) else {
        bail!("no audio device with UID {uid}");
    };
    open_id(id, uid.to_string(), direction)
}

fn open_id(id: AudioObjectID, uid: String, direction: Direction) -> Result<Device> {
    let format = virtual_format(id, direction)?;
    if format.mFormatID != kAudioFormatLinearPCM
        || format.mFormatFlags & kAudioFormatFlagIsFloat == 0
        || format.mBitsPerChannel != 32
    {
        bail!("{uid} does not present 32-bit float samples to its clients");
    }

    let channels = channel_count(id, direction.scope());
    if channels == 0 {
        bail!("{uid} has no {direction:?} channels");
    }
    let rate = number::<f64>(
        id,
        kAudioDevicePropertyNominalSampleRate,
        kAudioObjectPropertyScopeGlobal,
    )
    .unwrap_or(format.mSampleRate);
    if rate <= 0.0 {
        bail!("{uid} reports a sample rate of {rate}");
    }

    let block_frames = number::<u32>(
        id,
        kAudioDevicePropertyBufferFrameSize,
        kAudioObjectPropertyScopeGlobal,
    )
    .unwrap_or(512);
    let latency_frames = number::<u32>(id, kAudioDevicePropertyLatency, direction.scope())
        .unwrap_or(0)
        + number::<u32>(id, kAudioDevicePropertySafetyOffset, direction.scope()).unwrap_or(0)
        + stream_latency(id, direction)
        + block_frames;

    Ok(Device {
        id,
        uid,
        name: string(id, kAudioObjectPropertyName).unwrap_or_default(),
        direction,
        rate,
        channels: channels as usize,
        latency_frames,
        block_frames,
    })
}

/// Every device with channels in `direction`, for the CLI to print.
pub fn list(direction: Direction) -> Result<Vec<Device>> {
    let mut found = Vec::new();
    for id in device_ids()? {
        if channel_count(id, direction.scope()) == 0 {
            continue;
        }
        let Some(uid) = string(id, kAudioDevicePropertyDeviceUID) else {
            continue;
        };
        match open_id(id, uid, direction) {
            Ok(device) => found.push(device),
            Err(e) => tracing::debug!("skipping a device: {e:#}"),
        }
    }
    Ok(found)
}

/// A running IO proc. Dropping it stops the device and takes the callback with
/// it, so nothing outlives the buffers it borrows.
pub struct Stream {
    device: AudioObjectID,
    proc_id: AudioDeviceIOProcID,
    context: *mut Context,
}

// The callback is owned by this and only ever runs on the HAL's IO thread.
unsafe impl Send for Stream {}

struct Context {
    channels: usize,
    callback: Callback,
}

type ReadFrames = Box<dyn FnMut(&[f32]) + Send>;
type WriteFrames = Box<dyn FnMut(&mut [f32]) + Send>;

enum Callback {
    Input(ReadFrames),
    Output(WriteFrames),
}

impl Stream {
    /// Starts reading `device`. The callback is handed interleaved frames and
    /// runs on an audio thread, so it must not allocate, lock or log.
    pub fn input(device: &Device, callback: impl FnMut(&[f32]) + Send + 'static) -> Result<Self> {
        Self::start(device, Callback::Input(Box::new(callback)))
    }

    /// Starts writing `device`. The callback must fill every sample it is given.
    pub fn output(
        device: &Device,
        callback: impl FnMut(&mut [f32]) + Send + 'static,
    ) -> Result<Self> {
        Self::start(device, Callback::Output(Box::new(callback)))
    }

    fn start(device: &Device, callback: Callback) -> Result<Self> {
        if streams(device.id, device.direction).len() != 1 {
            bail!(
                "{} presents its {:?} channels as more than one stream",
                device.uid,
                device.direction
            );
        }

        let context = Box::into_raw(Box::new(Context {
            channels: device.channels,
            callback,
        }));
        let mut proc_id: AudioDeviceIOProcID = None;
        let status = unsafe {
            AudioDeviceCreateIOProcID(
                device.id,
                Some(io_proc),
                context.cast::<c_void>(),
                NonNull::from(&mut proc_id),
            )
        };
        if status != 0 || proc_id.is_none() {
            drop(unsafe { Box::from_raw(context) });
            bail!(
                "could not attach to {}: Core Audio returned {status}",
                device.uid
            );
        }

        let stream = Stream {
            device: device.id,
            proc_id,
            context,
        };
        let status = unsafe { AudioDeviceStart(device.id, proc_id) };
        if status != 0 {
            bail!(
                "could not start {}: Core Audio returned {status}",
                device.uid
            );
        }
        Ok(stream)
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        unsafe {
            AudioDeviceStop(self.device, self.proc_id);
            let status = AudioDeviceDestroyIOProcID(self.device, self.proc_id);
            if status == 0 {
                // Destroying the proc is synchronous, so by here the callback
                // has returned for the last time and its state can go.
                drop(Box::from_raw(self.context));
            } else {
                // The device is probably gone. Freeing state the HAL might
                // still reach is worse than leaking it.
                tracing::warn!("Core Audio returned {status} detaching an IO proc");
            }
        }
    }
}

unsafe extern "C-unwind" fn io_proc(
    _device: AudioObjectID,
    _now: NonNull<AudioTimeStamp>,
    input: NonNull<AudioBufferList>,
    _input_time: NonNull<AudioTimeStamp>,
    output: NonNull<AudioBufferList>,
    _output_time: NonNull<AudioTimeStamp>,
    context: *mut c_void,
) -> i32 {
    let context = unsafe { &mut *context.cast::<Context>() };
    let list = match context.callback {
        Callback::Input(_) => input,
        Callback::Output(_) => output,
    };

    // A device that has just gone away hands over an empty list rather than
    // failing, so this is a normal thing to see, not an error.
    if unsafe { list.as_ref() }.mNumberBuffers == 0 {
        return 0;
    }
    let buffer = unsafe { &*ptr::addr_of!((*list.as_ptr()).mBuffers).cast::<AudioBuffer>() };
    if buffer.mNumberChannels as usize != context.channels || buffer.mData.is_null() {
        return 0;
    }
    let samples = buffer.mDataByteSize as usize / size_of::<f32>();

    match &mut context.callback {
        Callback::Input(callback) => {
            callback(unsafe { std::slice::from_raw_parts(buffer.mData.cast::<f32>(), samples) })
        }
        Callback::Output(callback) => {
            callback(unsafe { std::slice::from_raw_parts_mut(buffer.mData.cast::<f32>(), samples) })
        }
    }
    0
}

fn device_ids() -> Result<Vec<AudioObjectID>> {
    let mut addr = address(
        kAudioHardwarePropertyDevices,
        kAudioObjectPropertyScopeGlobal,
    );
    let system = kAudioObjectSystemObject as AudioObjectID;
    let mut size = property_size(system, &mut addr)?;

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

fn streams(id: AudioObjectID, direction: Direction) -> Vec<AudioObjectID> {
    let mut addr = address(kAudioDevicePropertyStreams, direction.scope());
    let Ok(mut size) = property_size(id, &mut addr) else {
        return Vec::new();
    };
    let mut ids = vec![0 as AudioObjectID; size as usize / size_of::<AudioObjectID>()];
    if ids.is_empty() {
        return ids;
    }
    let status = unsafe {
        AudioObjectGetPropertyData(
            id,
            NonNull::from(&mut addr),
            0,
            ptr::null(),
            NonNull::from(&mut size),
            NonNull::new(ids.as_mut_ptr().cast::<c_void>()).unwrap(),
        )
    };
    if status != 0 {
        return Vec::new();
    }
    ids.truncate(size as usize / size_of::<AudioObjectID>());
    ids
}

fn stream_latency(id: AudioObjectID, direction: Direction) -> u32 {
    streams(id, direction)
        .first()
        .and_then(|stream| {
            number::<u32>(
                *stream,
                kAudioStreamPropertyLatency,
                kAudioObjectPropertyScopeGlobal,
            )
        })
        .unwrap_or(0)
}

/// The format the device presents to its clients, which is what an IO proc
/// hands over regardless of what the hardware does underneath.
fn virtual_format(id: AudioObjectID, direction: Direction) -> Result<AudioStreamBasicDescription> {
    let Some(stream) = streams(id, direction).first().copied() else {
        bail!("device has no {direction:?} stream");
    };
    number::<AudioStreamBasicDescription>(
        stream,
        kAudioStreamPropertyVirtualFormat,
        kAudioObjectPropertyScopeGlobal,
    )
    .ok_or_else(|| anyhow::anyhow!("device did not report its {direction:?} format"))
}

/// Sum of the channels on every stream the device has in one scope.
fn channel_count(id: AudioObjectID, scope: u32) -> u32 {
    let mut addr = address(kAudioDevicePropertyStreamConfiguration, scope);
    let Ok(mut size) = property_size(id, &mut addr) else {
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

fn number<T: Copy>(id: AudioObjectID, selector: u32, scope: u32) -> Option<T> {
    let mut addr = address(selector, scope);
    let mut value = std::mem::MaybeUninit::<T>::uninit();
    let mut size = size_of::<T>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            id,
            NonNull::from(&mut addr),
            0,
            ptr::null(),
            NonNull::from(&mut size),
            NonNull::new(value.as_mut_ptr().cast::<c_void>()).unwrap(),
        )
    };
    (status == 0 && size as usize == size_of::<T>()).then(|| unsafe { value.assume_init() })
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

fn property_size(id: AudioObjectID, addr: &mut AudioObjectPropertyAddress) -> Result<u32> {
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

    // Whatever is plugged into this machine, these have to hold of all of it.
    #[test]
    fn every_listed_device_can_be_found_again_by_uid() {
        for direction in [Direction::Input, Direction::Output] {
            for device in list(direction).unwrap() {
                assert_eq!(find(&device.uid), Some(device.id), "{}", device.uid);
            }
        }
    }

    #[test]
    fn a_uid_nobody_has_is_not_found() {
        assert_eq!(find("wsctl-no-such-device"), None);
    }

    #[test]
    fn an_open_device_reports_a_rate_and_channels() {
        for direction in [Direction::Input, Direction::Output] {
            for device in list(direction).unwrap() {
                assert!(
                    device.rate >= 8_000.0,
                    "{} runs at {}",
                    device.uid,
                    device.rate
                );
                assert!(device.channels > 0, "{} has no channels", device.uid);
                assert!(device.block_frames > 0, "{} has no block", device.uid);
            }
        }
    }
}
