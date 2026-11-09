//! The in-app half of herdr-ctrl: serves the window's frame record, in-process input,
//! screenshots, recordings, file panels and the clipboard over a Unix socket.
//!
//! Derived from the gpui-agent app crate (wsp-labs/gpui-agent, Apache-2.0). Needs the frame
//! record patch in `patches/` applied to gpui-pre.

#[cfg(target_os = "macos")]
mod conceal;

use anyhow::{Context as _, anyhow};
use gpui::{
    AnyWindowHandle, App, AppContext as _, ClipboardEntry, ClipboardItem, ExternalPaths,
    FileDropEvent, Global, Image, ImageFormat, KeyDownEvent, KeyUpEvent, Keystroke,
    ModifiersChangedEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    NavigationDirection, Pixels, PlatformInput, Point, ScrollDelta, ScrollWheelEvent, Window,
    point, px,
};
use herdr_ctrl_protocol::{
    Button, DropStage, ErrorKind, ErrorReply, Flags, Hello, InputEvent,
    PROTOCOL_VERSION, PickRecord, RawNode, RawText, RawTree, Recording, Rect, Request, Response,
    ScreenshotInfo, WindowInfo,
};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;

type Job = (Request, mpsc::Sender<Response>);

/// Serves herdr-ctrl on `socket`. `version` is reported to clients.
pub fn init(cx: &mut App, socket: PathBuf, version: &str) {
    #[cfg(target_os = "macos")]
    if std::env::var_os("HERDR_CTRL_HIDDEN").is_some() {
        conceal::start(cx);
    }
    if UnixStream::connect(&socket).is_ok() {
        eprintln!("herdr-ctrl: another window serves {}", socket.display());
        return;
    }
    let listener = match bind(&socket) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("herdr-ctrl: cannot listen on {}: {error:#}", socket.display());
            return;
        }
    };
    let (jobs, receiver) = async_channel::unbounded::<Job>();
    std::thread::Builder::new()
        .name("herdr-ctrl".into())
        .spawn(move || accept(listener, jobs))
        .expect("spawn herdr-ctrl thread");
    let picks = Picks::default();
    cx.set_global(picks.clone());
    cx.set_global(Version(version.to_string()));
    cx.set_path_prompt_hook(move |prompt| picks.intercept(prompt));
    cx.spawn(async move |cx| {
        while let Ok((request, reply)) = receiver.recv().await {
            let response = cx.update(|cx| handle(request, cx));
            reply.send(response).ok();
        }
    })
    .detach();
}

struct Version(String);

impl Global for Version {}

/// Answers file panels from a queue the command line fills. With nothing queued the native
/// panel opens as usual, so a live window keeps working for its user.
#[derive(Default, Clone)]
struct Picks(Rc<RefCell<PickState>>);

#[derive(Default)]
struct PickState {
    queue: VecDeque<Option<Vec<PathBuf>>>,
    log: Vec<PickRecord>,
}

impl Global for Picks {}

impl Picks {
    fn intercept(&self, prompt: gpui::PathPrompt) -> Option<Option<Vec<PathBuf>>> {
        let mut state = self.0.borrow_mut();
        let queued = state.queue.pop_front();
        let (kind, request) = match &prompt {
            gpui::PathPrompt::Open(options) => (
                "open",
                format!("files {} directories {} multiple {}", options.files, options.directories, options.multiple),
            ),
            gpui::PathPrompt::Save { directory, suggested_name } => (
                "save",
                format!("in {} named {}", directory.display(), suggested_name.as_deref().unwrap_or("(none)")),
            ),
        };
        state.log.push(PickRecord {
            kind: kind.into(),
            request,
            answer: queued.clone().flatten().map(|paths| paths.iter().map(|p| p.to_string_lossy().to_string()).collect()),
            unqueued: queued.is_none(),
        });
        queued
    }
}

fn bind(path: &Path) -> anyhow::Result<UnixListener> {
    if path.exists() {
        std::fs::remove_file(path).context("remove stale socket")?;
    }
    Ok(UnixListener::bind(path)?)
}

fn accept(listener: UnixListener, jobs: async_channel::Sender<Job>) {
    for stream in listener.incoming().flatten() {
        let jobs = jobs.clone();
        std::thread::spawn(move || serve(stream, jobs));
    }
}

fn serve(stream: UnixStream, jobs: async_channel::Sender<Job>) {
    let Ok(mut writer) = stream.try_clone() else { return };
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { return };
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(request) => {
                let (reply, answer) = mpsc::channel();
                if jobs.send_blocking((request, reply)).is_err() {
                    return;
                }
                match answer.recv() {
                    Ok(response) => response,
                    Err(_) => return,
                }
            }
            Err(error) => failure(ErrorKind::BadRequest, format!("bad request: {error}")),
        };
        let Ok(mut text) = serde_json::to_string(&response) else { return };
        text.push('\n');
        if writer.write_all(text.as_bytes()).is_err() {
            return;
        }
    }
}

fn failure(kind: ErrorKind, message: impl Into<String>) -> Response {
    Response::Err { error: ErrorReply { kind, message: message.into() } }
}

struct Refusal(ErrorKind, String);

impl From<anyhow::Error> for Refusal {
    fn from(error: anyhow::Error) -> Self {
        Refusal(ErrorKind::Failed, format!("{error:#}"))
    }
}

fn handle(request: Request, cx: &mut App) -> Response {
    let result = match request {
        Request::Hello => to_value(Hello {
            protocol: PROTOCOL_VERSION,
            pid: std::process::id(),
            os: std::env::consts::OS.into(),
            version: cx.try_global::<Version>().map(|v| v.0.clone()).unwrap_or_default(),
        }),
        Request::Windows => windows(cx),
        Request::Tree { window } => on_window(cx, window, tree),
        Request::Input { window, events } => on_window(cx, window, move |window, cx| input(window, cx, &events)),
        Request::Screenshot { window, path } => on_window(cx, window, move |window, cx| {
            settle(window, cx);
            screenshot(window, &path)
        }),
        Request::Pick { paths } => {
            let picks = cx.global::<Picks>().clone();
            let answer = (!paths.is_empty()).then(|| paths.into_iter().map(PathBuf::from).collect());
            picks.0.borrow_mut().queue.push_back(answer);
            Ok(serde_json::json!({ "queued": picks.0.borrow().queue.len() }))
        }
        Request::Picks => to_value(cx.global::<Picks>().0.borrow().log.clone()),
        Request::RecordStart { window, dir, fps } => record_start(cx, window, dir, fps),
        Request::RecordStop => record_stop(cx),
        Request::ClipboardSet { text, png } => clipboard_set(cx, text, png),
        Request::ClipboardGet { png } => clipboard_get(cx, png),
    };
    match result {
        Ok(ok) => Response::Ok { ok },
        Err(Refusal(kind, message)) => failure(kind, message),
    }
}

fn to_value(value: impl serde::Serialize) -> Result<serde_json::Value, Refusal> {
    serde_json::to_value(value).map_err(|error| Refusal(ErrorKind::Failed, error.to_string()))
}

fn find_window(cx: &App, id: u64) -> Result<AnyWindowHandle, Refusal> {
    cx.windows()
        .into_iter()
        .find(|handle| handle.window_id().as_u64() == id)
        .ok_or_else(|| Refusal(ErrorKind::NoWindow, format!("no window {id}")))
}

fn on_window(
    cx: &mut App,
    id: u64,
    f: impl FnOnce(&mut Window, &mut App) -> Result<serde_json::Value, Refusal>,
) -> Result<serde_json::Value, Refusal> {
    let handle = find_window(cx, id)?;
    cx.update_window(handle, |_, window, cx| f(window, cx))
        .map_err(|error| Refusal(ErrorKind::NoWindow, format!("{error:#}")))?
}

fn windows(cx: &mut App) -> Result<serde_json::Value, Refusal> {
    let active = cx.active_window().map(|handle| handle.window_id().as_u64());
    let mut list = Vec::new();
    for handle in cx.windows() {
        let id = handle.window_id().as_u64();
        let info = cx.update_window(handle, |_, window, _| {
            let size = window.viewport_size();
            WindowInfo {
                window: id,
                title: window.window_title(),
                active: active == Some(id) || window.is_window_active(),
                width: f32::from(size.width),
                height: f32::from(size.height),
                scale: window.scale_factor(),
            }
        });
        if let Ok(info) = info {
            list.push(info);
        }
    }
    to_value(list)
}

fn rect(bounds: gpui::Bounds<Pixels>) -> Rect {
    Rect {
        x: f32::from(bounds.origin.x),
        y: f32::from(bounds.origin.y),
        w: f32::from(bounds.size.width),
        h: f32::from(bounds.size.height),
    }
}

fn tree(window: &mut Window, cx: &mut App) -> Result<serde_json::Value, Refusal> {
    if !window.driver_enabled() {
        window.set_driver_enabled(true);
        window.refresh();
    }
    settle(window, cx);
    if window.driver_nodes().is_empty() {
        return Err(Refusal(ErrorKind::Warming, "waiting for the first recorded frame".into()));
    }
    if window.driver_frame_pending() {
        return Err(Refusal(ErrorKind::Warming, "waiting for the next frame".into()));
    }
    let nodes = window
        .driver_nodes()
        .iter()
        .map(|node| RawNode {
            parent: node.parent,
            bounds: rect(node.bounds),
            visible: rect(node.visible_bounds),
            id: node.element_id.as_ref().map(|id| id.to_string()),
            type_name: node.type_name.to_string(),
            source: node.source.map(|location| format!("{}:{}", location.file(), location.line())),
            role: node.role.map(|role| format!("{role:?}")),
            label: node.label.as_ref().map(|label| label.to_string()),
            value: node.value.as_ref().map(|value| value.to_string()),
            disabled: node.disabled,
            checked: node.toggled.map(|toggled| {
                match toggled {
                    gpui::Toggled::True => "checked",
                    gpui::Toggled::False => "unchecked",
                    gpui::Toggled::Mixed => "mixed",
                }
                .to_string()
            }),
            flags: Flags {
                click: node.flags.click,
                mouse: node.flags.mouse,
                keys: node.flags.keys,
                hover: node.flags.hover,
                focusable: node.flags.focusable,
                focused: node.flags.focused,
                scroll: node.flags.scroll,
                drag: node.flags.drag,
                drop: node.flags.drop,
                tooltip: node.flags.tooltip,
                blocks: node.flags.blocks,
            },
            key_context: node.key_context.as_ref().map(|context| context.to_string()),
            focus: node.focus.map(|focus| format!("{focus:?}")),
        })
        .collect();
    let texts = window
        .driver_nodes()
        .iter()
        .enumerate()
        .flat_map(|(index, node)| {
            node.texts.iter().map(move |text| RawText {
                bounds: rect(text.bounds),
                visible: rect(text.visible_bounds),
                text: text.text.to_string(),
                node: Some(index),
            })
        })
        .collect();
    let size = window.viewport_size();
    to_value(RawTree {
        window: window.window_handle().window_id().as_u64(),
        title: window.window_title(),
        width: f32::from(size.width),
        height: f32::from(size.height),
        scale: window.scale_factor(),
        nodes,
        texts,
        text_inputs: window.driver_text_inputs().iter().map(|focus| format!("{focus:?}")).collect(),
        prompt: window.has_active_prompt(),
        active: window.is_window_active(),
    })
}

const STALL: std::time::Duration = std::time::Duration::from_millis(200);

#[derive(Default)]
struct Stalls(std::collections::HashMap<u64, std::time::Instant>);

impl Global for Stalls {}

/// A hidden or minimized window stops getting frames from the platform. After `STALL`, draw
/// the pending frame here, as GPUI's own tests do.
fn settle(window: &mut Window, cx: &mut App) {
    let id = window.window_handle().window_id().as_u64();
    if !window.driver_frame_pending() {
        cx.default_global::<Stalls>().0.remove(&id);
        return;
    }
    let since = *cx.default_global::<Stalls>().0.entry(id).or_insert_with(std::time::Instant::now);
    if since.elapsed() >= STALL {
        window.draw(cx).clear(cx);
        cx.default_global::<Stalls>().0.remove(&id);
    }
}

fn position(x: f32, y: f32) -> Point<Pixels> {
    point(px(x), px(y))
}

fn modifiers(m: &herdr_ctrl_protocol::Modifiers) -> gpui::Modifiers {
    gpui::Modifiers { control: m.control, alt: m.alt, shift: m.shift, platform: m.platform, function: false }
}

fn button(button: Button) -> MouseButton {
    match button {
        Button::Left => MouseButton::Left,
        Button::Right => MouseButton::Right,
        Button::Middle => MouseButton::Middle,
        Button::Back => MouseButton::Navigate(NavigationDirection::Back),
        Button::Forward => MouseButton::Navigate(NavigationDirection::Forward),
    }
}

fn typed_keystroke(c: char) -> Keystroke {
    let key = match c {
        ' ' => "space".to_string(),
        '\n' => "enter".to_string(),
        '\t' => "tab".to_string(),
        c => c.to_lowercase().collect(),
    };
    Keystroke {
        modifiers: gpui::Modifiers { shift: c.is_uppercase(), ..Default::default() },
        key,
        key_char: Some(c.to_string()),
        ..Default::default()
    }
}

fn parse_keystroke(source: &str) -> Result<Keystroke, Refusal> {
    Keystroke::parse(source).map_err(|error| Refusal(ErrorKind::BadRequest, format!("bad keystroke {source:?}: {error}")))
}

fn input(window: &mut Window, cx: &mut App, events: &[InputEvent]) -> Result<serde_json::Value, Refusal> {
    let mut handled = Vec::new();
    for (index, event) in events.iter().enumerate() {
        let platform = match event {
            InputEvent::MouseMove { x, y, pressed, modifiers: m } => PlatformInput::MouseMove(MouseMoveEvent {
                position: position(*x, *y),
                pressed_button: pressed.map(button),
                modifiers: modifiers(m),
            }),
            InputEvent::MouseDown { x, y, button: b, click_count, modifiers: m } => PlatformInput::MouseDown(MouseDownEvent {
                button: button(*b),
                position: position(*x, *y),
                modifiers: modifiers(m),
                click_count: *click_count,
                first_mouse: false,
            }),
            InputEvent::MouseUp { x, y, button: b, click_count, modifiers: m } => PlatformInput::MouseUp(MouseUpEvent {
                button: button(*b),
                position: position(*x, *y),
                modifiers: modifiers(m),
                click_count: *click_count,
            }),
            InputEvent::Scroll { x, y, dx, dy, lines, modifiers: m } => PlatformInput::ScrollWheel(ScrollWheelEvent {
                position: position(*x, *y),
                delta: if *lines { ScrollDelta::Lines(point(*dx, *dy)) } else { ScrollDelta::Pixels(point(px(*dx), px(*dy))) },
                modifiers: modifiers(m),
                ..Default::default()
            }),
            InputEvent::Modifiers { modifiers: m } => {
                PlatformInput::ModifiersChanged(ModifiersChangedEvent { modifiers: modifiers(m), ..Default::default() })
            }
            InputEvent::KeyDown { keystroke, held } => PlatformInput::KeyDown(KeyDownEvent {
                keystroke: parse_keystroke(keystroke)?,
                is_held: *held,
                prefer_character_input: false,
            }),
            InputEvent::KeyUp { keystroke } => PlatformInput::KeyUp(KeyUpEvent { keystroke: parse_keystroke(keystroke)? }),
            InputEvent::FileDrop { stage, x, y, paths } => PlatformInput::FileDrop(match stage {
                DropStage::Enter => FileDropEvent::Entered {
                    position: position(*x, *y),
                    paths: ExternalPaths(paths.iter().map(PathBuf::from).collect()),
                },
                DropStage::Move => FileDropEvent::Pending { position: position(*x, *y) },
                DropStage::Submit => FileDropEvent::Submit { position: position(*x, *y) },
                DropStage::Exit => FileDropEvent::Exited,
            }),
            InputEvent::Key { keystroke } => {
                handled.push(window.dispatch_keystroke(parse_keystroke(keystroke)?, cx));
                continue;
            }
            InputEvent::Text { text, keys } => {
                if !keys && window.driver_insert_text(text, cx) {
                    handled.push(true);
                } else {
                    for c in text.chars() {
                        handled.push(window.dispatch_keystroke(typed_keystroke(c), cx));
                    }
                }
                continue;
            }
            InputEvent::ReplaceText { text } => {
                if !window.driver_replace_text(text, cx) {
                    return Err(Refusal(ErrorKind::NotFound, "no text field has focus; click one first".into()));
                }
                handled.push(true);
                continue;
            }
        };
        handled.push(!window.dispatch_event(platform, cx).propagate);
        if index + 1 < events.len() {
            window.draw(cx).clear(cx);
        }
    }
    to_value(serde_json::json!({ "handled": handled }))
}

fn screenshot(window: &mut Window, path: &str) -> Result<serde_json::Value, Refusal> {
    let image = window.render_to_image().map_err(|error| anyhow!("render_to_image: {error:#}"))?;
    image.save(path).with_context(|| format!("write {path}"))?;
    to_value(ScreenshotInfo { path: path.into(), width: image.width(), height: image.height() })
}

fn clipboard_set(cx: &mut App, text: Option<String>, png: Option<String>) -> Result<serde_json::Value, Refusal> {
    let item = match (text, png) {
        (Some(text), None) => ClipboardItem::new_string(text),
        (None, Some(path)) => {
            let bytes = std::fs::read(&path).with_context(|| format!("read {path}"))?;
            ClipboardItem::new_image(&Image::from_bytes(ImageFormat::Png, bytes))
        }
        _ => return Err(Refusal(ErrorKind::BadRequest, "give text or a PNG path".into())),
    };
    cx.write_to_clipboard(item);
    Ok(serde_json::Value::Null)
}

fn clipboard_get(cx: &mut App, png: Option<String>) -> Result<serde_json::Value, Refusal> {
    let Some(item) = cx.read_from_clipboard() else {
        return Ok(serde_json::json!({ "empty": true }));
    };
    let mut entries = Vec::new();
    for entry in item.entries() {
        match entry {
            ClipboardEntry::String(string) => entries.push(serde_json::json!({ "text": string.text() })),
            ClipboardEntry::Image(image) => {
                let saved = match &png {
                    Some(path) => {
                        std::fs::write(path, &image.bytes).with_context(|| format!("write {path}"))?;
                        Some(path.clone())
                    }
                    None => None,
                };
                entries.push(serde_json::json!({ "image": format!("{:?}", image.format), "bytes": image.bytes.len(), "saved": saved }));
            }
            other => entries.push(serde_json::json!({ "other": format!("{other:?}") })),
        }
    }
    Ok(serde_json::json!({ "entries": entries }))
}

#[derive(Default, Clone)]
struct Recorder(Rc<RefCell<RecorderState>>);

#[derive(Default)]
struct RecorderState {
    task: Option<gpui::Task<()>>,
    dir: String,
    fps: u32,
    frames: Rc<std::cell::Cell<usize>>,
    started: Option<std::time::Instant>,
}

impl Global for Recorder {}

fn record_start(cx: &mut App, window: u64, dir: String, fps: u32) -> Result<serde_json::Value, Refusal> {
    let handle = find_window(cx, window)?;
    let fps = fps.clamp(1, 30);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {dir}"))?;
    if cx.try_global::<Recorder>().is_some_and(|recorder| recorder.0.borrow().task.is_some()) {
        return Err(Refusal(ErrorKind::BadRequest, "already recording".into()));
    }
    let frames = Rc::new(std::cell::Cell::new(0));
    let task = cx.spawn({
        let dir = dir.clone();
        let frames = frames.clone();
        async move |cx| {
            let interval = std::time::Duration::from_millis(1000 / fps as u64);
            let mut next = std::time::Instant::now();
            loop {
                let image = cx.update_window(handle, |_, window, cx| {
                    settle(window, cx);
                    window.render_to_image()
                });
                let Ok(Ok(image)) = image else { break };
                let path = format!("{dir}/frame-{:05}.png", frames.get());
                frames.set(frames.get() + 1);
                cx.background_spawn(async move {
                    image.save(&path).ok();
                })
                .detach();
                next += interval;
                let now = std::time::Instant::now();
                if next > now {
                    cx.background_executor().timer(next - now).await;
                } else {
                    next = now;
                }
            }
        }
    });
    let recorder = cx.default_global::<Recorder>().clone();
    *recorder.0.borrow_mut() =
        RecorderState { task: Some(task), dir: dir.clone(), fps, frames, started: Some(std::time::Instant::now()) };
    to_value(serde_json::json!({ "dir": dir, "fps": fps }))
}

fn record_stop(cx: &mut App) -> Result<serde_json::Value, Refusal> {
    let recorder = cx.try_global::<Recorder>().cloned();
    match recorder {
        Some(recorder) if recorder.0.borrow().task.is_some() => {
            let mut state = recorder.0.borrow_mut();
            state.task = None;
            to_value(Recording {
                dir: state.dir.clone(),
                frames: state.frames.get(),
                fps: state.fps,
                seconds: state.started.map(|started| started.elapsed().as_secs_f32()).unwrap_or_default(),
            })
        }
        _ => Err(Refusal(ErrorKind::NotFound, "nothing is recording".into())),
    }
}

