//! Profile icon names → Segoe MDL2 Assets code points (present on Windows 10 and 11).

pub const FONT: &str = "Segoe MDL2 Assets";

/// Every icon name a profile may use.
pub const ICONS: &[&str] = &[
    "mute", "vol-down", "vol-up", "mic-mute", "bright-down", "bright-up", "kbd-light", "display", "touchpad", "airplane", "camera",
    "lock", "sleep", "screenshot", "calculator", "settings", "search", "app", "fan", "performance", "refresh-rate", "fnlock", "call",
    "call-end", "favorites", "emoji", "copilot", "ai", "play-pause", "prev", "next", "stop", "home", "end", "pgup", "pgdn", "insert",
    "prtsc", "scrlk", "pause", "break", "sysrq", "numlock", "rotate", "battery", "wifi", "privacy", "power", "info", "other",
];

/// Keys like Home/PgUp/Break read better as text, so they have no glyph on purpose.
pub fn glyph(icon: &str) -> Option<char> {
    Some(match icon {
        "mute" => '\u{E74F}',
        "vol-down" => '\u{E993}',
        "vol-up" => '\u{E995}',
        "mic-mute" => '\u{EC54}',
        "bright-down" => '\u{EC8A}',
        "bright-up" => '\u{E706}',
        "kbd-light" => '\u{E765}',
        "display" => '\u{E7F4}',
        "touchpad" => '\u{EFA5}',
        "airplane" => '\u{E709}',
        "camera" => '\u{E714}',
        "lock" | "fnlock" => '\u{E72E}',
        "sleep" => '\u{E708}',
        "screenshot" => '\u{E7C5}',
        "calculator" => '\u{E8EF}',
        "settings" => '\u{E713}',
        "search" => '\u{E721}',
        "app" => '\u{E71D}',
        "fan" => '\u{E9CA}',
        "performance" => '\u{EC4A}',
        "refresh-rate" => '\u{E72C}',
        "call" => '\u{E717}',
        "call-end" => '\u{E778}',
        "favorites" => '\u{E734}',
        "emoji" => '\u{E76E}',
        "copilot" | "ai" => '\u{E99A}',
        "play-pause" => '\u{E768}',
        "prev" => '\u{E892}',
        "next" => '\u{E893}',
        "stop" => '\u{E71A}',
        "rotate" => '\u{E7AD}',
        "battery" => '\u{E83F}',
        "wifi" => '\u{E701}',
        "privacy" => '\u{E727}',
        "power" => '\u{E7E8}',
        "info" => '\u{E946}',
        "other" => '\u{E765}',
        _ => return None,
    })
}
