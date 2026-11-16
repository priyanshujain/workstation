pub const PAGE: &str = include_str!("../assets/share.html");
pub const LOGIN: &str = include_str!("../assets/login.html");

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
