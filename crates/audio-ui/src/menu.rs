//! The one menu the app has.
//!
//! It is rebuilt from scratch every time it is about to open, which is what
//! keeps it honest about devices that were plugged in or unplugged while the
//! app was already running.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSControlStateValue, NSControlStateValueOff, NSControlStateValueOn,
    NSEventTrackingRunLoopMode, NSMenu, NSMenuDelegate, NSMenuItem,
};
use objc2_foundation::{
    MainThreadMarker, NSObject, NSObjectProtocol, NSRunLoop, NSString, NSTimer, ns_string,
};

use crate::bridge::{self, Device};
use crate::settings::Settings;

/// The voice gate as the menu offers it. The number is a probability nobody
/// should have to think in, so it is never shown. 0.8 is the one the
/// suppressor's own gate tests are written against: it shuts on room noise
/// without taking the front off a word.
const GATES: [(&str, f32); 4] = [
    ("Off", 0.0),
    ("Light", 0.5),
    ("Medium", 0.8),
    ("Strong", 0.95),
];

/// How near a stored threshold has to be to a preset to be shown as that
/// preset. The presets are far enough apart that this cannot be ambiguous.
const GATE_TOLERANCE: f32 = 0.01;

/// How wide the voice meter is, and how often it is redrawn while the menu is
/// up. The width is fixed because a menu sizes itself as it opens and will not
/// grow around a title that got longer underneath it.
const METER_CELLS: usize = 10;
const METER_INTERVAL: f64 = 0.1;

pub struct Ivars {
    settings: RefCell<Settings>,
    inputs: RefCell<Vec<Device>>,
    outputs: RefCell<Vec<Device>>,
    /// The voice meter, on the opens where there is one. Held so the timer can
    /// rewrite its title without hunting for it in the menu.
    meter: RefCell<Option<Retained<NSMenuItem>>>,
    timer: RefCell<Option<Retained<NSTimer>>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "WsctlAudioController"]
    #[ivars = Ivars]
    pub struct Controller;

    impl Controller {
        #[unsafe(method(toggleBridge:))]
        fn toggle_bridge(&self, _sender: &NSMenuItem) {
            if bridge::is_running() {
                if let Err(e) = bridge::stop() {
                    tracing::error!("could not stop the bridge: {e:#}");
                }
                return;
            }
            let settings = self.ivars().settings.borrow();
            let (Some(input), Some(output)) = (&settings.input_uid, &settings.output_uid) else {
                return;
            };
            if let Err(e) = bridge::start(input, output) {
                tracing::error!("could not start the bridge: {e:#}");
            }
        }

        #[unsafe(method(selectInput:))]
        fn select_input(&self, sender: &NSMenuItem) {
            let uid = self.ivars().inputs.borrow().get(index(sender)).map(|d| d.uid.clone());
            self.choose(|s| &mut s.input_uid, uid);
        }

        #[unsafe(method(selectOutput:))]
        fn select_output(&self, sender: &NSMenuItem) {
            let uid = self.ivars().outputs.borrow().get(index(sender)).map(|d| d.uid.clone());
            self.choose(|s| &mut s.output_uid, uid);
        }

        #[unsafe(method(toggleDenoise:))]
        fn toggle_denoise(&self, _sender: &NSMenuItem) {
            let on = !bridge::denoise();
            bridge::set_denoise(on);
            self.remember(|s| s.denoise = Some(on));
        }

        #[unsafe(method(selectVoiceGate:))]
        fn select_voice_gate(&self, sender: &NSMenuItem) {
            let Some(&(_, threshold)) = GATES.get(index(sender)) else {
                return;
            };
            bridge::set_voice_threshold(threshold);
            self.remember(|s| s.voice_threshold = Some(threshold));
        }

        #[unsafe(method(tick:))]
        fn tick(&self, _timer: &NSTimer) {
            if let Some(item) = self.ivars().meter.borrow().as_ref() {
                item.setTitle(&NSString::from_str(&meter(bridge::voice().unwrap_or(0.0))));
            }
        }

        #[unsafe(method(quit:))]
        fn quit(&self, _sender: &NSMenuItem) {
            if bridge::is_running() {
                let _ = bridge::stop();
            }
            NSApplication::sharedApplication(self.mtm()).terminate(None);
        }
    }

    unsafe impl NSObjectProtocol for Controller {}

    unsafe impl NSMenuDelegate for Controller {
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, menu: &NSMenu) {
            self.build(menu);
        }

        // An open menu holds the run loop in event tracking mode, where a
        // timer scheduled the ordinary way never fires, so the meter's goes
        // into that mode by hand. It lives exactly as long as the menu is up.
        #[unsafe(method(menuWillOpen:))]
        fn menu_will_open(&self, _menu: &NSMenu) {
            if self.ivars().meter.borrow().is_none() {
                return;
            }
            let timer = unsafe {
                NSTimer::timerWithTimeInterval_target_selector_userInfo_repeats(
                    METER_INTERVAL,
                    self as &AnyObject,
                    sel!(tick:),
                    None,
                    true,
                )
            };
            unsafe {
                NSRunLoop::currentRunLoop().addTimer_forMode(&timer, NSEventTrackingRunLoopMode)
            };
            *self.ivars().timer.borrow_mut() = Some(timer);
        }

        #[unsafe(method(menuDidClose:))]
        fn menu_did_close(&self, _menu: &NSMenu) {
            if let Some(timer) = self.ivars().timer.borrow_mut().take() {
                timer.invalidate();
            }
            self.ivars().meter.borrow_mut().take();
        }
    }
);

impl Controller {
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            settings: RefCell::new(Settings::load()),
            inputs: RefCell::new(Vec::new()),
            outputs: RefCell::new(Vec::new()),
            meter: RefCell::new(None),
            timer: RefCell::new(None),
        });
        unsafe { msg_send![super(this), init] }
    }

    /// Hands the engine back the noise settings the user left. They are
    /// process-wide rather than engine state, so this holds before anything
    /// has been started and through every device change afterwards.
    pub fn restore(&self) {
        let settings = self.ivars().settings.borrow();
        bridge::set_denoise(settings.denoise.unwrap_or(false));
        bridge::set_voice_threshold(settings.voice_threshold.unwrap_or(0.0));
    }

    pub fn build(&self, menu: &NSMenu) {
        let mtm = self.mtm();
        menu.removeAllItems();
        menu.setAutoenablesItems(false);

        let running = bridge::is_running();

        let settings = self.ivars().settings.borrow();
        let picked = settings.input_uid.is_some() && settings.output_uid.is_some();

        // There is no window to explain a greyed-out item, so the title says why.
        let title = if running || picked {
            "Bridge on"
        } else {
            "Bridge on (choose a microphone and speaker)"
        };
        let toggle = self.item(mtm, title, Some(sel!(toggleBridge:)));
        toggle.setState(state(running));
        toggle.setEnabled(running || picked);
        menu.addItem(&toggle);
        menu.addItem(&NSMenuItem::separatorItem(mtm));

        let inputs = list(bridge::input_devices());
        let outputs = list(bridge::output_devices());
        menu.addItem(&self.submenu(
            mtm,
            "Microphone",
            &inputs,
            settings.input_uid.as_deref(),
            sel!(selectInput:),
        ));
        menu.addItem(&self.submenu(
            mtm,
            "Speaker",
            &outputs,
            settings.output_uid.as_deref(),
            sel!(selectOutput:),
        ));
        drop(settings);
        *self.ivars().inputs.borrow_mut() = inputs;
        *self.ivars().outputs.borrow_mut() = outputs;

        menu.addItem(&NSMenuItem::separatorItem(mtm));
        let denoising = bridge::denoise();
        let reduce = self.item(mtm, "Reduce noise", Some(sel!(toggleDenoise:)));
        reduce.setState(state(denoising));
        menu.addItem(&reduce);
        menu.addItem(&self.gate(mtm, denoising));

        // The only sign the app can give that the model is hearing anything.
        // It is only there while the suppressor is running, so a menu with the
        // bridge off looks exactly as it did before.
        *self.ivars().meter.borrow_mut() = (running && denoising).then(|| {
            let item = self.item(mtm, &meter(bridge::voice().unwrap_or(0.0)), None);
            item.setEnabled(false);
            menu.addItem(&item);
            item
        });

        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&self.item(mtm, "Quit", Some(sel!(quit:))));
    }

    /// The voice gate. Its threshold is compared against the model's opinion of
    /// the audio, so it can only do anything while the suppressor is running.
    /// As with "Bridge on", the title carries the reason rather than leaving a
    /// submenu that quietly does nothing.
    fn gate(&self, mtm: MainThreadMarker, denoising: bool) -> Retained<NSMenuItem> {
        let menu = NSMenu::new(mtm);
        menu.setAutoenablesItems(false);

        let current = bridge::voice_threshold();
        for (i, &(name, threshold)) in GATES.iter().enumerate() {
            let entry = self.item(mtm, name, Some(sel!(selectVoiceGate:)));
            entry.setTag(i as isize);
            entry.setState(state((current - threshold).abs() < GATE_TOLERANCE));
            menu.addItem(&entry);
        }

        let title = if denoising {
            "Mute when not speaking"
        } else {
            "Mute when not speaking (needs Reduce noise)"
        };
        let parent = self.item(mtm, title, None);
        parent.setSubmenu(Some(&menu));
        parent.setEnabled(denoising);
        parent
    }

    fn submenu(
        &self,
        mtm: MainThreadMarker,
        title: &str,
        devices: &[Device],
        selected: Option<&str>,
        action: Sel,
    ) -> Retained<NSMenuItem> {
        let menu = NSMenu::new(mtm);
        menu.setAutoenablesItems(false);

        if devices.is_empty() {
            let empty = self.item(mtm, "No devices", None);
            empty.setEnabled(false);
            menu.addItem(&empty);
        }
        for (i, device) in devices.iter().enumerate() {
            let entry = self.item(mtm, &device.name, Some(action));
            entry.setTag(i as isize);
            entry.setState(state(selected == Some(device.uid.as_str())));
            menu.addItem(&entry);
        }

        let parent = self.item(mtm, title, None);
        parent.setSubmenu(Some(&menu));
        parent
    }

    fn item(
        &self,
        mtm: MainThreadMarker,
        title: &str,
        action: Option<Sel>,
    ) -> Retained<NSMenuItem> {
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(title),
                action,
                ns_string!(""),
            )
        };
        if action.is_some() {
            unsafe { item.setTarget(Some(self as &AnyObject)) };
        }
        item
    }

    fn remember(&self, edit: impl FnOnce(&mut Settings)) {
        let mut settings = self.ivars().settings.borrow_mut();
        edit(&mut settings);
        if let Err(e) = settings.save() {
            tracing::error!("could not save the setting: {e:#}");
        }
    }

    /// Records a choice and, if the bridge is already up, moves it onto the new
    /// endpoint rather than leaving it on the one that was just deselected.
    fn choose(&self, field: fn(&mut Settings) -> &mut Option<String>, uid: Option<String>) {
        let Some(uid) = uid else { return };
        let mut settings = self.ivars().settings.borrow_mut();
        *field(&mut settings) = Some(uid);
        if let Err(e) = settings.save() {
            tracing::error!("could not save the selection: {e:#}");
        }
        let restart = bridge::is_running()
            .then(|| Some((settings.input_uid.clone()?, settings.output_uid.clone()?)))
            .flatten();
        drop(settings);

        if let Some((input, output)) = restart {
            let _ = bridge::stop();
            if let Err(e) = bridge::start(&input, &output) {
                tracing::error!("could not restart the bridge: {e:#}");
            }
        }
    }
}

fn index(item: &NSMenuItem) -> usize {
    item.tag().max(0) as usize
}

fn state(on: bool) -> NSControlStateValue {
    if on {
        NSControlStateValueOn
    } else {
        NSControlStateValueOff
    }
}

fn meter(voice: f32) -> String {
    let filled = (voice.clamp(0.0, 1.0) * METER_CELLS as f32).round() as usize;
    let mut title = String::from("Voice  ");
    title.extend((0..METER_CELLS).map(|cell| if cell < filled { '█' } else { '░' }));
    title
}

fn list(devices: anyhow::Result<Vec<Device>>) -> Vec<Device> {
    devices.unwrap_or_else(|e| {
        tracing::error!("could not read the device list: {e:#}");
        Vec::new()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // The menu can only be looked at, so what is left to test is the arithmetic
    // underneath it: the presets and the bar.

    #[test]
    fn off_is_the_first_thing_offered_and_it_is_the_default() {
        assert_eq!(GATES[0], ("Off", 0.0));
    }

    #[test]
    fn no_two_presets_are_near_enough_to_both_be_ticked() {
        for &(name, threshold) in &GATES {
            let near = GATES
                .iter()
                .filter(|(_, other)| (threshold - other).abs() < GATE_TOLERANCE)
                .count();
            assert_eq!(near, 1, "{name} is close enough to another preset to tie");
        }
    }

    // A preset that came back from JSON as 0.7999999 would leave the submenu
    // with nothing ticked and no way to tell what is in force.
    #[test]
    fn a_preset_is_still_itself_after_a_trip_through_the_settings_file() {
        for &(name, threshold) in &GATES {
            let written = serde_json::to_string(&threshold).unwrap();
            let read: f32 = serde_json::from_str(&written).unwrap();
            assert!(
                (threshold - read).abs() < GATE_TOLERANCE,
                "{name} was written as {written} and came back as {read}"
            );
        }
    }

    #[test]
    fn the_meter_is_the_same_width_however_loud_it_is() {
        let width = meter(0.0).chars().count();
        for step in 0..=20 {
            let title = meter(step as f32 / 20.0);
            assert_eq!(title.chars().count(), width, "{title} is a different width");
        }
    }

    #[test]
    fn the_meter_fills_as_the_voice_does() {
        let filled = |voice| meter(voice).chars().filter(|c| *c == '█').count();
        assert_eq!(filled(0.0), 0);
        assert_eq!(filled(1.0), METER_CELLS);
        assert!((1..METER_CELLS).contains(&filled(0.5)));
        assert_eq!(filled(2.0), METER_CELLS, "a bad reading ran off the end");
        assert_eq!(filled(-1.0), 0, "a bad reading ran off the other end");
    }
}
