use std::fmt;

use anyhow::{Result, bail};

/// How adb reaches a device.
///
/// The same phone appears twice, once per transport, whenever `adb tcpip` has been armed and
/// the cable is still plugged in. Every bare adb command then fails with "more than one
/// device", which is why [`resolve`] collapses the pair rather than reporting both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Usb,
    Network,
    Emulator,
}

/// Connection state from the second column of `adb devices`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Ready,
    Unauthorized,
    Offline,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub serial: String,
    pub state: State,
    pub transport: Transport,
    /// `model:` from `adb devices -l`, absent when adb ran without `-l`.
    pub model: Option<String>,
    /// `device:` from `adb devices -l`, the board name. Stable across transports, so it is
    /// what tells us two entries are one phone.
    pub board: Option<String>,
}

impl Device {
    /// One-line form for `wsctl android list`.
    pub fn label(&self) -> String {
        let name = self.model.as_deref().unwrap_or("unknown");
        format!(
            "{}  {name}  [{}, {}]",
            self.serial, self.transport, self.state
        )
    }

    pub fn is_ready(&self) -> bool {
        self.state == State::Ready
    }

    pub fn is_emulator(&self) -> bool {
        self.transport == Transport::Emulator
    }
}

/// Parse `adb devices -l` output. Unparseable lines are skipped rather than failing the whole
/// listing, since adb interleaves daemon startup chatter with the table.
pub fn parse_devices(output: &str) -> Vec<Device> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| {
            !line.is_empty() && !line.starts_with("List of devices") && !line.starts_with('*')
        })
        .filter_map(parse_line)
        .collect()
}

fn parse_line(line: &str) -> Option<Device> {
    let mut parts = line.split_whitespace();
    let serial = parts.next()?;
    let state = parts.next()?;

    let mut model = None;
    let mut board = None;
    for field in parts {
        match field.split_once(':') {
            Some(("model", value)) => model = Some(value.to_string()),
            Some(("device", value)) => board = Some(value.to_string()),
            _ => {}
        }
    }

    Some(Device {
        serial: serial.to_string(),
        state: parse_state(state),
        transport: transport_for(serial),
        model,
        board,
    })
}

fn parse_state(raw: &str) -> State {
    match raw {
        "device" => State::Ready,
        "unauthorized" => State::Unauthorized,
        "offline" => State::Offline,
        other => State::Other(other.to_string()),
    }
}

fn transport_for(serial: &str) -> Transport {
    if serial.starts_with("emulator-") {
        Transport::Emulator
    } else if is_host_port(serial) {
        Transport::Network
    } else {
        Transport::Usb
    }
}

/// Wireless adb serials are `host:port`. Nothing else adb reports carries a numeric suffix
/// after a colon, so this is enough to tell the transports apart.
fn is_host_port(serial: &str) -> bool {
    serial
        .rsplit_once(':')
        .is_some_and(|(host, port)| !host.is_empty() && port.parse::<u16>().is_ok())
}

/// Pick the device to drive.
///
/// Physical hardware beats emulators, which are usually running alongside and are never what
/// you want to mirror. When one phone is reachable on both transports the cabled entry wins,
/// keeping traffic on the faster and steadier link.
pub fn resolve<'a>(devices: &'a [Device], wanted: Option<&str>) -> Result<&'a Device> {
    if let Some(serial) = wanted {
        return resolve_exact(devices, serial);
    }

    let physical: Vec<&Device> = devices
        .iter()
        .filter(|d| d.is_ready() && !d.is_emulator())
        .collect();

    match prefer_cabled(&physical).as_slice() {
        [] => bail!("{}", nothing_usable(devices)),
        [only] => Ok(only),
        many => {
            let list = many
                .iter()
                .map(|d| d.label())
                .collect::<Vec<_>>()
                .join("\n  ");
            bail!("more than one device connected, pass --device:\n  {list}")
        }
    }
}

fn resolve_exact<'a>(devices: &'a [Device], serial: &str) -> Result<&'a Device> {
    let Some(found) = devices.iter().find(|d| d.serial == serial) else {
        let known = devices
            .iter()
            .map(|d| d.serial.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        if known.is_empty() {
            bail!("no device {serial:?}, and adb sees nothing connected");
        }
        bail!("no device {serial:?}, adb sees: {known}");
    };

    if !found.is_ready() {
        bail!("device {serial} is {}", found.state);
    }
    Ok(found)
}

/// Drop the network entry when the same board is also cabled.
fn prefer_cabled<'a>(devices: &[&'a Device]) -> Vec<&'a Device> {
    devices
        .iter()
        .copied()
        .filter(|candidate| {
            candidate.transport != Transport::Network
                || !devices.iter().any(|other| {
                    other.transport == Transport::Usb
                        && other.board.is_some()
                        && other.board == candidate.board
                })
        })
        .collect()
}

/// Explain an empty result in terms of what adb actually saw, since "no device" alone sends
/// you looking at the cable when the real cause is usually an unaccepted debugging prompt.
fn nothing_usable(devices: &[Device]) -> String {
    let blocked: Vec<String> = devices
        .iter()
        .filter(|d| !d.is_emulator() && !d.is_ready())
        .map(|d| format!("{} ({})", d.serial, d.state))
        .collect();
    if !blocked.is_empty() {
        return format!("no usable device: {}", blocked.join(", "));
    }

    let emulators = devices.iter().filter(|d| d.is_emulator()).count();
    if emulators > 0 {
        return format!(
            "no physical device connected, ignoring {emulators} emulator(s); pass --device to target one"
        );
    }
    "no device connected".to_string()
}

impl fmt::Display for Transport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Transport::Usb => "usb",
            Transport::Network => "network",
            Transport::Emulator => "emulator",
        };
        f.write_str(name)
    }
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            State::Ready => f.write_str("ready"),
            State::Unauthorized => f.write_str("unauthorized, accept the prompt on the device"),
            State::Offline => f.write_str("offline"),
            State::Other(raw) => f.write_str(raw),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real output from the dock setup: phone on USB, same phone over wireless adb, emulator.
    const DUAL_HOMED: &str = "\
List of devices attached
663c91b1               device usb:2-1.1.3.2.2 product:CPH2467 model:CPH2467 device:OP5958L1 transport_id:17
192.168.1.243:5555     device product:CPH2467 model:CPH2467 device:OP5958L1 transport_id:18
emulator-5554          device product:sdk_gphone64_arm64 model:sdk_gphone64_arm64 device:emu64a transport_id:7";

    #[test]
    fn parses_a_real_listing() {
        let devices = parse_devices(DUAL_HOMED);
        assert_eq!(devices.len(), 3);
        assert_eq!(devices[0].serial, "663c91b1");
        assert_eq!(devices[0].model.as_deref(), Some("CPH2467"));
        assert_eq!(devices[0].board.as_deref(), Some("OP5958L1"));
        assert!(devices.iter().all(Device::is_ready));
    }

    #[test]
    fn classifies_transports_by_serial_shape() {
        let devices = parse_devices(DUAL_HOMED);
        assert_eq!(devices[0].transport, Transport::Usb);
        assert_eq!(devices[1].transport, Transport::Network);
        assert_eq!(devices[2].transport, Transport::Emulator);
    }

    #[test]
    fn one_phone_on_two_transports_resolves_to_the_cable() {
        let devices = parse_devices(DUAL_HOMED);
        let picked = resolve(&devices, None).unwrap();
        assert_eq!(picked.serial, "663c91b1");
        assert_eq!(picked.transport, Transport::Usb);
    }

    #[test]
    fn emulators_are_never_picked_implicitly() {
        let only_emulator = parse_devices("emulator-5554  device model:sdk_gphone64_arm64");
        let err = resolve(&only_emulator, None).unwrap_err().to_string();
        assert!(err.contains("no physical device"), "{err}");
        assert!(err.contains("--device"), "{err}");
    }

    #[test]
    fn emulators_can_still_be_targeted_explicitly() {
        let devices = parse_devices(DUAL_HOMED);
        let picked = resolve(&devices, Some("emulator-5554")).unwrap();
        assert_eq!(picked.transport, Transport::Emulator);
    }

    #[test]
    fn two_distinct_phones_need_disambiguation() {
        let devices = parse_devices(
            "663c91b1  device model:CPH2467 device:OP5958L1\n\
             9a8b7c6d  device model:Pixel_8 device:shiba",
        );
        let err = resolve(&devices, None).unwrap_err().to_string();
        assert!(err.contains("more than one device"), "{err}");
        assert!(
            err.contains("663c91b1") && err.contains("9a8b7c6d"),
            "{err}"
        );
    }

    #[test]
    fn distinct_boards_are_not_collapsed_across_transports() {
        // Only a shared board name means one phone; two different devices must both survive.
        let devices = parse_devices(
            "663c91b1           device model:CPH2467 device:OP5958L1\n\
             192.168.1.99:5555  device model:Pixel_8 device:shiba",
        );
        assert!(resolve(&devices, None).is_err());
    }

    #[test]
    fn unauthorized_devices_explain_themselves() {
        let devices = parse_devices("663c91b1  unauthorized");
        let err = resolve(&devices, None).unwrap_err().to_string();
        assert!(err.contains("unauthorized"), "{err}");
        assert!(err.contains("accept the prompt"), "{err}");
    }

    #[test]
    fn offline_devices_are_not_silently_chosen() {
        let devices = parse_devices("663c91b1  offline model:CPH2467 device:OP5958L1");
        assert!(resolve(&devices, None).is_err());
        assert!(resolve(&devices, Some("663c91b1")).is_err());
    }

    #[test]
    fn unknown_serial_lists_what_adb_saw() {
        let devices = parse_devices(DUAL_HOMED);
        let err = resolve(&devices, Some("nope")).unwrap_err().to_string();
        assert!(err.contains("663c91b1"), "{err}");
    }

    #[test]
    fn daemon_chatter_is_ignored() {
        let devices = parse_devices(
            "* daemon not running; starting now at tcp:5037\n\
             * daemon started successfully\n\
             List of devices attached\n\
             663c91b1  device model:CPH2467 device:OP5958L1",
        );
        assert_eq!(devices.len(), 1);
    }

    #[test]
    fn empty_listing_yields_nothing() {
        assert!(parse_devices("List of devices attached\n").is_empty());
        assert!(resolve(&[], None).is_err());
        assert!(resolve(&[], Some("663c91b1")).is_err());
    }

    #[test]
    fn hostnames_and_ipv4_both_read_as_network() {
        assert_eq!(transport_for("192.168.1.243:5555"), Transport::Network);
        assert_eq!(transport_for("phone.local:5555"), Transport::Network);
        assert_eq!(transport_for("663c91b1"), Transport::Usb);
        // A trailing colon with no port is not a wireless serial.
        assert_eq!(transport_for("weird:"), Transport::Usb);
    }
}
