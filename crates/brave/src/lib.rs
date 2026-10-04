use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};

const POLICIES: &[(&str, bool)] = &[
    ("BraveP3AEnabled", false),
    ("BraveStatsPingEnabled", false),
    ("BraveRewardsDisabled", true),
    ("BraveWalletDisabled", true),
    ("BraveVPNDisabled", true),
    ("BraveAIChatEnabled", false),
    ("BraveLocalAIEnabled", false),
    ("BraveNewsDisabled", true),
    ("BraveTalkDisabled", true),
    ("BravePlaylistEnabled", false),
    ("BraveWebDiscoveryEnabled", false),
    ("BraveSpeedreaderEnabled", false),
    ("BraveWaybackMachineEnabled", false),
    ("TorDisabled", true),
    ("EmailAliasesEnabled", false),
    ("MetricsReportingEnabled", false),
    ("SafeBrowsingExtendedReportingEnabled", false),
    ("UrlKeyedAnonymizedDataCollectionEnabled", false),
];

fn profile_xml() -> String {
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         <key>PayloadContent</key>\n\
         <array>\n\
         <dict>\n\
         <key>PayloadType</key><string>com.apple.ManagedClient.preferences</string>\n\
         <key>PayloadVersion</key><integer>1</integer>\n\
         <key>PayloadIdentifier</key><string>dev.pj.workstation.brave.policies</string>\n\
         <key>PayloadUUID</key><string>58D8496F-4B78-4919-9D32-F948A57435F2</string>\n\
         <key>PayloadContent</key>\n\
         <dict>\n\
         <key>com.brave.Browser</key>\n\
         <dict>\n\
         <key>Forced</key>\n\
         <array>\n\
         <dict>\n\
         <key>mcx_preference_settings</key>\n\
         <dict>\n",
    );

    for (name, enabled) in POLICIES {
        let value = if *enabled { "true" } else { "false" };
        xml.push_str(&format!("<key>{name}</key><{value}/>\n"));
    }

    xml.push_str(
        "</dict>\n\
         </dict>\n\
         </array>\n\
         </dict>\n\
         </dict>\n\
         </dict>\n\
         </array>\n\
         <key>PayloadType</key><string>Configuration</string>\n\
         <key>PayloadVersion</key><integer>1</integer>\n\
         <key>PayloadIdentifier</key><string>dev.pj.workstation.brave</string>\n\
         <key>PayloadUUID</key><string>98765EF0-4F62-47B9-9FE7-9E157E2293C1</string>\n\
         <key>PayloadDisplayName</key><string>Workstation Brave Policy</string>\n\
         <key>PayloadScope</key><string>System</string>\n\
         </dict>\n\
         </plist>\n",
    );

    xml
}

pub fn write_profile(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("creating Brave configuration profile at {}", path.display()))?;
    file.write_all(profile_xml().as_bytes())
        .with_context(|| format!("writing Brave configuration profile to {}", path.display()))
}
