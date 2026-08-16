//! Menu bar app for the wsctl audio bridge: pick the real microphone and
//! speaker, switch the bridge on. There is no window, and there never is one.

mod bridge;
mod coreaudio;
mod menu;
mod settings;

use objc2::runtime::ProtocolObject;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSImage, NSMenu, NSStatusBar,
    NSVariableStatusItemLength,
};
use objc2_foundation::{MainThreadMarker, ns_string};

use menu::Controller;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let mtm = MainThreadMarker::new().expect("main() is not on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    // Accessory keeps it out of the Dock even when the binary is run bare,
    // without the bundle's LSUIElement.
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let controller = Controller::new(mtm);
    // Before anything is started, so the first bridge of the session already
    // treats the microphone the way the last one was left.
    controller.restore();

    let menu = NSMenu::new(mtm);
    menu.setDelegate(Some(ProtocolObject::from_ref(&*controller)));
    controller.build(&menu);

    let status_item =
        NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    if let Some(button) = status_item.button(mtm) {
        match NSImage::imageWithSystemSymbolName_accessibilityDescription(
            ns_string!("waveform"),
            Some(ns_string!("wsctl audio")),
        ) {
            Some(image) => {
                image.setTemplate(true);
                button.setImage(Some(&image));
            }
            None => button.setTitle(ns_string!("wsctl")),
        }
    }
    status_item.setMenu(Some(&menu));

    app.run();
}
