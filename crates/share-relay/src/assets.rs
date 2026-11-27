use std::sync::LazyLock;

use sha2::{Digest, Sha256};

pub static PAGE: LazyLock<String> =
    LazyLock::new(|| versioned(include_str!("../assets/share.html")));
pub static LOGIN: LazyLock<String> =
    LazyLock::new(|| versioned(include_str!("../assets/login.html")));

const FILES: &[(&str, &str, &str)] = &[
    (
        "xterm.js",
        "text/javascript; charset=utf-8",
        include_str!("../assets/xterm.js"),
    ),
    (
        "xterm.css",
        "text/css; charset=utf-8",
        include_str!("../assets/xterm.css"),
    ),
    (
        "share.js",
        "text/javascript; charset=utf-8",
        include_str!("../assets/share.js"),
    ),
    (
        "share.css",
        "text/css; charset=utf-8",
        include_str!("../assets/share.css"),
    ),
];

pub fn find(name: &str) -> Option<(&'static str, &'static str)> {
    FILES
        .iter()
        .find(|(file, _, _)| *file == name)
        .map(|(_, content_type, body)| (*content_type, *body))
}

fn versioned(page: &str) -> String {
    let version = version();
    FILES.iter().fold(page.to_string(), |page, (name, _, _)| {
        page.replace(
            &format!("/assets/{name}\""),
            &format!("/assets/{name}?v={version}\""),
        )
    })
}

fn version() -> String {
    let mut hasher = Sha256::new();
    for (_, _, body) in FILES {
        hasher.update(body.as_bytes());
    }
    hasher.finalize()[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
