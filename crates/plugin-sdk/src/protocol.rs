//! Message types of the bar ⇄ plugin protocol (v1). One JSON object per line.

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

/// Computed CSS text style of a slot.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Style {
    pub color: String,
    pub font_family: String,
    pub font_size: f64,
    pub font_weight: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Init {
    pub protocol: u32,
    pub instance: String,
    pub bar: String,
    pub output: String,
    pub edge: String,
    pub orientation: String,
    pub width: f64,
    pub height: f64,
    pub scale: f64,
    pub style: Style,
    pub config: serde_json::Value,
    pub config_dir: String,
}

/// Mouse button: 1 left, 2 middle, 3 right, 8/9 back/forward.
pub type Button = u32;

/// Pointer state carried by every mouse message except `mouseleave`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Pointer {
    /// Position in SVG user units of the frame on screen.
    pub x: f64,
    pub y: f64,
    /// False when the pointer is in the slot but outside the drawn picture.
    pub inside: bool,
    /// Ids of SVG elements under the pointer, innermost first.
    pub targets: Vec<String>,
    /// Held buttons as a DOM-style bitmask (1 left, 2 right, 4 middle).
    pub buttons: u32,
    pub mods: Vec<String>,
}

impl Pointer {
    /// First target starting with `prefix`, with the prefix removed: `target("ws-")` → `Some("3")`.
    pub fn target(&self, prefix: &str) -> Option<&str> {
        self.targets.iter().find_map(|t| t.strip_prefix(prefix))
    }
}

/// Messages from the bar to a plugin (stdin).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum BarMsg {
    Init(Init),
    Resize { width: f64, height: f64, scale: f64 },
    Style(Style),
    MouseEnter(Pointer),
    MouseMove(Pointer),
    MouseDown { #[serde(flatten)] pointer: Pointer, button: Button },
    MouseUp { #[serde(flatten)] pointer: Pointer, button: Button },
    Click { #[serde(flatten)] pointer: Pointer, button: Button },
    MouseLeave,
    Wheel { #[serde(flatten)] pointer: Pointer, dx: f64, dy: f64 },
    /// Unknown message types are ignored (forward compatibility).
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Messages from a plugin to the bar (stdout).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum PluginMsg {
    Svg { svg: String },
    Tooltip {
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        anchor: Option<Rect>,
    },
    Log { level: String, message: String },
    #[serde(other)]
    Unknown,
}

/// Output of `dool-plugin-<name> --describe`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Description {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub protocol: u32,
    #[serde(default)]
    pub version: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_messages_round_trip_with_flattened_pointer() {
        let line = r#"{"type":"click","x":1.5,"y":2,"inside":true,"targets":["ev-3"],"buttons":0,"mods":["ctrl"],"button":1}"#;
        let msg: BarMsg = serde_json::from_str(line).unwrap();
        let BarMsg::Click { pointer, button } = &msg else { panic!("{msg:?}") };
        assert_eq!((pointer.x, *button, pointer.target("ev-")), (1.5, 1, Some("3")));
        let again: BarMsg = serde_json::from_str(&serde_json::to_string(&msg).unwrap()).unwrap();
        assert_eq!(again, msg);
    }

    #[test]
    fn unknown_types_and_fields_are_ignored() {
        let msg: BarMsg = serde_json::from_str(r#"{"type":"future","x":1}"#).unwrap();
        assert_eq!(msg, BarMsg::Unknown);
        let msg: BarMsg = serde_json::from_str(r#"{"type":"mouseleave","extra":true}"#).unwrap();
        assert_eq!(msg, BarMsg::MouseLeave);
    }

    #[test]
    fn tooltip_null_hides() {
        let msg: PluginMsg = serde_json::from_str(r#"{"type":"tooltip","text":null}"#).unwrap();
        assert_eq!(msg, PluginMsg::Tooltip { text: None, anchor: None });
    }
}
