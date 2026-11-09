//! The messages between the `herdr-ctrl` command line and the server inside a Herdr window.
//!
//! One JSON object per line over a Unix socket. Coordinates are logical pixels in window space.
//! Derived from the gpui-agent protocol (wsp-labs/gpui-agent, Apache-2.0).

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

/// The socket's file name inside the app's home (`$HERDR_APP_HOME`).
pub const SOCKET_NAME: &str = "herdr-ctrl.sock";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Hello,
    Windows,
    Tree {
        window: u64,
    },
    Input {
        window: u64,
        events: Vec<InputEvent>,
    },
    Screenshot {
        window: u64,
        path: String,
    },
    /// Queue the answer to the next file panel: paths, or a cancel when `paths` is empty.
    Pick {
        paths: Vec<String>,
    },
    /// The file panels the app asked for, oldest first.
    Picks,
    /// Start saving a PNG of the window `fps` times a second into `dir`.
    RecordStart {
        window: u64,
        dir: String,
        fps: u32,
    },
    RecordStop,
    ClipboardSet {
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        png: Option<String>,
    },
    ClipboardGet {
        #[serde(default)]
        png: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recording {
    pub dir: String,
    pub frames: usize,
    pub fps: u32,
    pub seconds: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PickRecord {
    pub kind: String,
    pub request: String,
    pub answer: Option<Vec<String>>,
    pub unqueued: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Response {
    Ok { ok: serde_json::Value },
    Err { error: ErrorReply },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorReply {
    pub kind: ErrorKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    Warming,
    NoWindow,
    NotFound,
    BadRequest,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u32,
    pub pid: u32,
    pub os: String,
    #[serde(default)]
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowInfo {
    pub window: u64,
    pub title: String,
    pub active: bool,
    pub width: f32,
    pub height: f32,
    pub scale: f32,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn area(&self) -> f32 {
        self.w.max(0.) * self.h.max(0.)
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0. || self.h <= 0.
    }

    pub fn center(&self) -> (f32, f32) {
        self.at(0.5, 0.5)
    }

    pub fn at(&self, fx: f32, fy: f32) -> (f32, f32) {
        (self.x + self.w * fx, self.y + self.h * fy)
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }

    pub fn contains_rect(&self, other: &Rect) -> bool {
        other.x >= self.x - 0.5
            && other.y >= self.y - 0.5
            && other.x + other.w <= self.x + self.w + 0.5
            && other.y + other.h <= self.y + self.h + 0.5
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Flags {
    #[serde(default, skip_serializing_if = "is_false")]
    pub click: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub mouse: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub keys: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub hover: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub focusable: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub focused: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub scroll: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub drag: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub drop: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub tooltip: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub blocks: bool,
}

impl Flags {
    pub fn pointer(&self) -> bool {
        self.click || self.mouse || self.hover || self.scroll || self.drag || self.drop || self.tooltip
    }

    pub fn interactive(&self) -> bool {
        self.click || self.mouse || self.focusable || self.scroll || self.drag || self.drop
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawNode {
    pub parent: Option<usize>,
    pub bounds: Rect,
    pub visible: Rect,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub type_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub disabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked: Option<String>,
    #[serde(default)]
    pub flags: Flags,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_context: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawText {
    pub bounds: Rect,
    pub visible: Rect,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawTree {
    pub window: u64,
    pub title: String,
    pub width: f32,
    pub height: f32,
    pub scale: f32,
    pub nodes: Vec<RawNode>,
    pub texts: Vec<RawText>,
    #[serde(default)]
    pub text_inputs: Vec<String>,
    #[serde(default)]
    pub prompt: bool,
    #[serde(default)]
    pub active: bool,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Modifiers {
    #[serde(default, skip_serializing_if = "is_false")]
    pub shift: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub control: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub alt: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub platform: bool,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Button {
    #[default]
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DropStage {
    #[default]
    Enter,
    Move,
    Submit,
    Exit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputEvent {
    MouseMove {
        x: f32,
        y: f32,
        #[serde(default)]
        pressed: Option<Button>,
        #[serde(default)]
        modifiers: Modifiers,
    },
    MouseDown {
        x: f32,
        y: f32,
        #[serde(default)]
        button: Button,
        #[serde(default = "one")]
        click_count: usize,
        #[serde(default)]
        modifiers: Modifiers,
    },
    MouseUp {
        x: f32,
        y: f32,
        #[serde(default)]
        button: Button,
        #[serde(default = "one")]
        click_count: usize,
        #[serde(default)]
        modifiers: Modifiers,
    },
    Scroll {
        x: f32,
        y: f32,
        dx: f32,
        dy: f32,
        #[serde(default)]
        lines: bool,
        #[serde(default)]
        modifiers: Modifiers,
    },
    /// One keystroke in GPUI's syntax, such as `enter`, `ctrl-a` or `ctrl-shift-p`.
    Key { keystroke: String },
    KeyDown {
        keystroke: String,
        #[serde(default)]
        held: bool,
    },
    KeyUp { keystroke: String },
    /// The set of held modifier keys changed, as when Ctrl is pressed or released alone.
    Modifiers { modifiers: Modifiers },
    /// Text typed into the focused field at its selection, as an input method would. With
    /// `keys`, or when no text field has focus, it is typed as one keystroke per character.
    Text {
        text: String,
        #[serde(default)]
        keys: bool,
    },
    ReplaceText { text: String },
    /// One stage of dragging files from another app over the window.
    FileDrop {
        stage: DropStage,
        #[serde(default)]
        x: f32,
        #[serde(default)]
        y: f32,
        #[serde(default)]
        paths: Vec<String>,
    },
}

fn one() -> usize {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenshotInfo {
    pub path: String,
    pub width: u32,
    pub height: u32,
}
