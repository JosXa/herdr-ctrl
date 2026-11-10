mod annotate;
mod fail;
mod instance;
mod mcp;
mod select;
mod snapshot;
mod target;

use crate::fail::{Fail, fail};
use crate::instance::{Instance, SpawnOptions, state_root};
use crate::select::{Refs, aim, describe, find_all, find_one};
use crate::snapshot::{Frame, Options};
use crate::target::{Ctrl, Target, home_dir, is_kind};
use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use herdr_ctrl_protocol::{Button, DropStage, InputEvent, Modifiers, PickRecord, RawTree, Recording, Request, ScreenshotInfo};
use serde_json::{Value, json};
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

const SKILL: &str = include_str!("../../../SKILL.md");

#[derive(Parser)]
#[command(
    name = "herdr-ctrl",
    version,
    about = "Drive Herdr like a browser: read its element tree, click and type in-process, and run hidden instances",
    after_help = "Run `herdr-ctrl skill` for the agent guide."
)]
struct Cli {
    /// Which Herdr: `live` (your window) or the name of a hidden instance.
    #[arg(long, global = true, env = "HERDR_CTRL_ON", default_value = "live")]
    on: String,
    /// Who is driving; keeps refs and scratch spaces apart between agents.
    #[arg(long, global = true, env = "HERDR_CTRL_AGENT", default_value = "agent")]
    agent: String,
    /// Print JSON on stdout, and errors as {"error": {kind, message, hint}}.
    #[arg(long, global = true)]
    json: bool,
    /// Send input to the live window outside your scratch space.
    #[arg(long, global = true)]
    allow_live: bool,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start a hidden Herdr with its own home and core, on Xvfb (Linux) or concealed (macOS).
    Spawn(SpawnArgs),
    /// List hidden instances.
    Instances,
    /// Show an instance's pids, paths, display and lease.
    Inspect,
    /// Tear hidden instances down: the window, its core and its display.
    Stop {
        names: Vec<String>,
        #[arg(long)]
        all: bool,
    },
    /// Remove instances whose supervisor is gone.
    Gc,
    /// Restart the window, picking up a rebuilt binary; the live window always keeps its core.
    Restart {
        /// Keep a hidden instance's core and its terminals running.
        #[arg(long)]
        keep_core: bool,
    },
    /// Run a command with the instance's environment, such as `exec -- hc ls`.
    Exec {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        command: Vec<String>,
    },
    /// The app's log (hidden instances only).
    Logs {
        #[arg(short = 'n', long, default_value_t = 100)]
        lines: usize,
        #[arg(short, long)]
        follow: bool,
    },
    /// URLs and files the instance asked the system to open.
    Opened,

    /// Herdr's state: space, tab, focused pane, layout, overlays, mod errors.
    Status,
    /// Spaces, tabs and panes.
    Ls,
    /// Run a Herdr command, such as `do space.new name=test`.
    Do {
        command: String,
        /// key=value arguments; values are read as JSON when they parse, else as text.
        args: Vec<String>,
    },
    /// The commands `do` accepts.
    Commands,
    /// Print a terminal's screen and history (`hc read`).
    Read { terminal: String },
    /// Paste text into a terminal and press enter (`hc send`).
    Send { terminal: String, text: String },

    /// The element tree as an outline with refs.
    Snapshot(SnapshotArgs),
    /// Elements a selector matches.
    Find { selector: String },
    /// Everything known about one element.
    Get { selector: String },
    /// Save the window, or one element, as a PNG.
    Screenshot {
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[arg(long)]
        element: Option<String>,
        /// Draw numbered boxes for the interactive refs.
        #[arg(long)]
        annotate: bool,
    },

    /// Click an element or a point `x,y`.
    Click {
        target: String,
        #[arg(long, value_enum, default_value = "left")]
        button: ButtonArg,
        #[arg(long)]
        double: bool,
        /// Held modifiers, such as ctrl,shift.
        #[arg(long, value_delimiter = ',')]
        with: Vec<String>,
        /// Where inside the element, as fractions of its size: 0.1,0.5.
        #[arg(long, value_parser = parse_fraction)]
        at: Option<(f32, f32)>,
    },
    /// Move the pointer over an element or a point.
    Hover {
        target: String,
        #[arg(long, value_parser = parse_fraction)]
        at: Option<(f32, f32)>,
    },
    /// Press on one element or point and release on another.
    Drag {
        from: String,
        to: String,
        #[arg(long, value_enum, default_value = "left")]
        button: ButtonArg,
        #[arg(long, value_delimiter = ',')]
        with: Vec<String>,
        #[arg(long, default_value_t = 10)]
        steps: usize,
        #[arg(long, value_parser = parse_fraction)]
        from_at: Option<(f32, f32)>,
        #[arg(long, value_parser = parse_fraction)]
        to_at: Option<(f32, f32)>,
    },
    /// Drop files on an element, as if dragged from a file manager.
    Drop {
        target: String,
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },
    /// Scroll the wheel over an element (or the window's middle).
    Scroll {
        #[arg(value_enum)]
        direction: Direction,
        #[arg(default_value_t = 3.0)]
        amount: f32,
        #[arg(long = "in")]
        within: Option<String>,
        /// Amount is pixels instead of lines.
        #[arg(long)]
        pixels: bool,
        #[arg(long, value_delimiter = ',')]
        with: Vec<String>,
    },
    /// Type text into the focused field, or into --into after clicking it.
    Type {
        text: String,
        #[arg(long)]
        into: Option<String>,
        /// Send one keystroke per character, as a terminal expects.
        #[arg(long)]
        keys: bool,
    },
    /// Click a field and replace its whole text.
    Fill { target: String, text: String },
    /// Press keys in GPUI syntax: enter, escape, ctrl-shift-p, alt-1.
    Press {
        #[arg(required = true)]
        keys: Vec<String>,
    },
    /// Send raw input events from a JSON file (or - for stdin); held keys and buttons are released on failure.
    Input { file: String },

    /// Answer the next file panel with these paths, or cancel it when none are given.
    Pick { paths: Vec<PathBuf> },
    /// The file panels the app asked for.
    Picks,
    /// Read or set the instance's clipboard.
    Clipboard {
        #[command(subcommand)]
        action: ClipboardCmd,
    },
    /// Wait for idle (the default), an element, or text.
    Wait {
        selector: Option<String>,
        #[arg(long)]
        text: Option<String>,
        /// Wait until the element or text is gone.
        #[arg(long)]
        gone: bool,
        #[arg(long, default_value = "10s")]
        timeout: String,
    },
    /// Fail with exit code 5 unless the condition holds now.
    Expect {
        selector: String,
        #[arg(long)]
        gone: bool,
        /// The element's text contains this.
        #[arg(long)]
        text: Option<String>,
        /// The element has this state, such as focused or disabled.
        #[arg(long)]
        state: Option<String>,
        /// Exactly this many elements match.
        #[arg(long)]
        count: Option<usize>,
    },
    /// Record the window to a video.
    Record {
        #[command(subcommand)]
        action: RecordCmd,
    },
    /// A space of your own in the live window, where input is allowed.
    Scratch {
        #[command(subcommand)]
        action: ScratchCmd,
    },
    /// Install, check and restore mods.
    Mod {
        #[command(subcommand)]
        action: ModCmd,
    },
    /// What this build can and cannot do.
    Capabilities,
    /// Print the agent guide.
    Skill,
    /// Serve every command as one MCP tool over stdio.
    Mcp,
    #[command(hide = true, name = "_supervise")]
    Supervise { name: String },
}

#[derive(Args)]
struct SpawnArgs {
    name: Option<String>,
    /// The herdr-app binary; default: HERDR_CTRL_BIN, else herdr-app on PATH.
    #[arg(long, conflicts_with = "build")]
    bin: Option<PathBuf>,
    /// Build this herdr-app checkout first and run its debug binary.
    #[arg(long)]
    build: Option<PathBuf>,
    #[arg(long, requires = "build")]
    release: bool,
    /// Start with an empty home instead of a copy of the live config.
    #[arg(long)]
    fresh: bool,
    /// Copy this mod folder into the instance's mods.
    #[arg(long = "mod")]
    mods: Vec<PathBuf>,
    #[arg(long, default_value = "1400x900", value_parser = parse_size)]
    size: (u32, u32),
    /// Tear down after this long without a command: 90s, 30m, 2h, or 0 for never.
    #[arg(long, default_value = "30m")]
    ttl: String,
    #[arg(long, value_parser = parse_env)]
    env: Vec<(String, String)>,
    /// HERDR_APP_LOG filter, such as debug or herdr_app=trace.
    #[arg(long)]
    log: Option<String>,
}

#[derive(Args)]
struct SnapshotArgs {
    /// Interactive elements only.
    #[arg(short, long)]
    interactive: bool,
    /// Include elements that are hidden or scrolled away.
    #[arg(long)]
    all: bool,
    #[arg(long)]
    depth: Option<usize>,
    /// Only the subtree of this element.
    #[arg(long)]
    within: Option<String>,
    /// Show where each element was built.
    #[arg(long)]
    source: bool,
}

#[derive(Subcommand)]
enum ClipboardCmd {
    Set {
        #[arg(long, conflicts_with = "image", required_unless_present = "image")]
        text: Option<String>,
        #[arg(long)]
        image: Option<PathBuf>,
    },
    Get {
        /// Save an image on the clipboard to this PNG.
        #[arg(long)]
        image: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum RecordCmd {
    Start {
        #[arg(long, default_value_t = 10)]
        fps: u32,
    },
    Stop {
        /// .mp4 or .gif; needs ffmpeg.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum ScratchCmd {
    Open,
    Close,
}

#[derive(Subcommand)]
enum ModCmd {
    Ls,
    /// Load a mod folder and render it once, without a window.
    Check { dir: PathBuf },
    /// Copy a mod folder into the Herdr's mods; the window reloads it. On live, the installed copy is backed up.
    Load {
        dir: PathBuf,
        #[arg(long = "as")]
        name: Option<String>,
    },
    /// Put back the copy `mod load` replaced.
    Restore { name: String },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum ButtonArg {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum Direction {
    Up,
    Down,
    Left,
    Right,
}

fn parse_fraction(text: &str) -> Result<(f32, f32), String> {
    parse_pair(text).ok_or_else(|| format!("{text:?} is not like 0.5,0.5"))
}

fn parse_pair(text: &str) -> Option<(f32, f32)> {
    let (a, b) = text.split_once(',')?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

fn parse_size(text: &str) -> Result<(u32, u32), String> {
    let (w, h) = text.split_once('x').ok_or("use WIDTHxHEIGHT")?;
    Ok((w.parse().map_err(|_| "bad width")?, h.parse().map_err(|_| "bad height")?))
}

fn parse_env(text: &str) -> Result<(String, String), String> {
    let (k, v) = text.split_once('=').ok_or("use KEY=VALUE")?;
    Ok((k.into(), v.into()))
}

impl From<ButtonArg> for Button {
    fn from(button: ButtonArg) -> Self {
        match button {
            ButtonArg::Left => Button::Left,
            ButtonArg::Right => Button::Right,
            ButtonArg::Middle => Button::Middle,
            ButtonArg::Back => Button::Back,
            ButtonArg::Forward => Button::Forward,
        }
    }
}

fn main() {
    let cli = Cli::parse();
    let json = cli.json;
    if let Err(error) = run(cli) {
        let (kind, message, hint, code) = match error.downcast_ref::<Fail>() {
            Some(f) => (f.kind, f.message.clone(), f.hint.clone(), f.exit_code()),
            None => ("failed", format!("{error:#}"), None, 1),
        };
        if json {
            println!("{}", json!({"error": {"kind": kind, "message": message, "hint": hint}}));
        } else {
            eprintln!("error ({kind}): {message}");
            if let Some(hint) = hint {
                eprintln!("hint: {hint}");
            }
        }
        std::process::exit(code);
    }
}

struct Out {
    json: bool,
}

impl Out {
    fn value(&self, value: &Value) {
        if self.json {
            println!("{value}");
        } else {
            println!("{}", serde_json::to_string_pretty(value).unwrap_or_default());
        }
    }

    fn done(&self, value: Value, human: impl AsRef<str>) {
        if self.json {
            println!("{value}");
        } else {
            println!("{}", human.as_ref());
        }
    }
}

struct Session {
    target: Target,
    ctrl: Ctrl,
    refs: Refs,
    window: u64,
}

impl Session {
    fn open(cli: &Cli) -> Result<Session> {
        let target = Target::resolve(&cli.on)?;
        let mut ctrl = target.ctrl()?;
        let window = ctrl.window()?.window;
        let refs = Refs::open(&target.name, &cli.agent, ctrl.hello.pid, window);
        Ok(Session {
            target,
            ctrl,
            refs,
            window,
        })
    }

    fn tree(&mut self) -> Result<RawTree> {
        self.ctrl.tree()
    }

    fn send(&mut self, events: Vec<InputEvent>) -> Result<Vec<bool>> {
        let reply: Value = self.ctrl.call(&Request::Input {
            window: self.window,
            events,
        })?;
        Ok(serde_json::from_value(reply["handled"].clone()).unwrap_or_default())
    }

    fn point(&mut self, target: &str, at: Option<(f32, f32)>) -> Result<((f32, f32), String)> {
        if let Some(point) = parse_pair(target) {
            return Ok((point, format!("({}, {})", point.0, point.1)));
        }
        let tree = self.tree()?;
        let entry = find_one(&tree, &mut self.refs, target)?;
        Ok((aim(&tree, &entry, at)?, describe(&entry)))
    }
}

fn modifiers(names: &[String]) -> Result<Modifiers> {
    let mut modifiers = Modifiers::default();
    for name in names {
        match name.trim().to_lowercase().as_str() {
            "shift" => modifiers.shift = true,
            "ctrl" | "control" => modifiers.control = true,
            "alt" | "option" => modifiers.alt = true,
            "super" | "cmd" | "meta" | "platform" | "win" => modifiers.platform = true,
            "" => {}
            other => return Err(fail("usage", format!("{other:?} is not a modifier: use ctrl, shift, alt or super"))),
        }
    }
    Ok(modifiers)
}

fn keystroke(key: &str) -> String {
    if key.len() > 1 && key.contains('+') && !key.ends_with('+') {
        key.replace('+', "-")
    } else {
        key.to_string()
    }
}

fn click_events(x: f32, y: f32, button: Button, double: bool, modifiers: Modifiers) -> Vec<InputEvent> {
    let mut events = vec![InputEvent::MouseMove {
        x,
        y,
        pressed: None,
        modifiers,
    }];
    for click_count in 1..=(if double { 2 } else { 1 }) {
        events.push(InputEvent::MouseDown {
            x,
            y,
            button,
            click_count,
            modifiers,
        });
        events.push(InputEvent::MouseUp {
            x,
            y,
            button,
            click_count,
            modifiers,
        });
    }
    events
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Scratch {
    space: String,
    previous: Option<String>,
}

fn scratch_path(cli: &Cli) -> PathBuf {
    state_root().join("scratch").join(format!("{}.{}.json", cli.on, cli.agent))
}

fn current_space(target: &Target) -> Result<Option<String>> {
    Ok(target.app(json!({"status": true}))?["space"].as_str().map(str::to_owned))
}

fn guard(cli: &Cli, target: &Target) -> Result<()> {
    if !target.is_live() || cli.allow_live {
        return Ok(());
    }
    let scratch: Option<Scratch> = fs::read_to_string(scratch_path(cli))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok());
    if let Some(scratch) = scratch
        && current_space(target)?.as_deref() == Some(scratch.space.as_str())
    {
        return Ok(());
    }
    Err(Fail::new("live_guard", "input to the live window is only allowed while your scratch space is selected")
        .hint("run `herdr-ctrl scratch open`, test in a hidden instance (`herdr-ctrl spawn`), or pass --allow-live")
        .into())
}

fn input_session(cli: &Cli) -> Result<Session> {
    let session = Session::open(cli)?;
    guard(cli, &session.target)?;
    Ok(session)
}

fn run(cli: Cli) -> Result<()> {
    let out = Out { json: cli.json };
    match &cli.command {
        Cmd::Spawn(args) => spawn(&out, &cli, args),
        Cmd::Instances => {
            let list: Vec<Value> = instance::list().iter().map(instance_summary).collect();
            if out.json {
                out.value(&Value::Array(list));
            } else if list.is_empty() {
                println!("no hidden instances");
            } else {
                for i in &list {
                    println!(
                        "{}  {}  display {}  pid {}  expires in {}",
                        i["name"].as_str().unwrap_or_default(),
                        i["state"].as_str().unwrap_or_default(),
                        i["display"].as_str().unwrap_or("-"),
                        i["app_pid"],
                        i["expires_in"].as_u64().map(|s| format!("{}m{}s", s / 60, s % 60)).unwrap_or("never".into()),
                    );
                }
            }
            Ok(())
        }
        Cmd::Inspect => {
            let target = Target::resolve(&cli.on)?;
            match &target.instance {
                Some(instance) => {
                    let mut value = serde_json::to_value(instance)?;
                    value["summary"] = instance_summary(instance);
                    out.value(&value)
                }
                None => out.value(&json!({"name": "live", "home": target.home, "bin": target.bin().ok()})),
            }
            Ok(())
        }
        Cmd::Stop { names, all } => {
            let names: Vec<String> = if *all {
                instance::list().into_iter().map(|i| i.name).collect()
            } else if !names.is_empty() {
                names.clone()
            } else if cli.on != target::LIVE {
                vec![cli.on.clone()]
            } else {
                return Err(fail("usage", "name the instances to stop, or pass --all; the live window is never stopped"));
            };
            for name in &names {
                if name == target::LIVE {
                    return Err(fail("usage", "the live window is never stopped"));
                }
                Instance::load(name)?;
                instance::stop(name, Duration::from_secs(20));
            }
            out.done(json!({"stopped": names}), format!("stopped {}", names.join(", ")));
            Ok(())
        }
        Cmd::Gc => {
            let removed = instance::gc();
            out.done(json!({"removed": removed}), format!("removed {} stale instances", removed.len()));
            Ok(())
        }
        Cmd::Restart { keep_core } => {
            let target = Target::resolve(&cli.on)?;
            let Some(mut instance) = target.instance else {
                return restart_live(&out, &target);
            };
            let mut ctrl = instance::restart(&mut instance, *keep_core)?;
            out.done(
                json!({"name": instance.name, "app_pid": ctrl.hello.pid, "generation": instance.generation, "window": ctrl.window()?.window}),
                format!("restarted {} (pid {})", instance.name, ctrl.hello.pid),
            );
            Ok(())
        }
        Cmd::Exec { command } => {
            let target = Target::resolve(&cli.on)?;
            let status = target.command(&command[0]).args(&command[1..]).status()?;
            std::process::exit(status.code().unwrap_or(1));
        }
        Cmd::Logs { lines, follow } => {
            let target = Target::resolve(&cli.on)?;
            let Some(instance) = &target.instance else {
                return Err(Fail::new("unsupported", "the live window's log is not reachable from here")
                    .hint("spawn a hidden instance to read its log")
                    .into());
            };
            let mut tail = std::process::Command::new("tail");
            tail.args(["-n", &lines.to_string()]);
            if *follow {
                tail.arg("-f");
            }
            tail.arg(instance.log_path()).status()?;
            Ok(())
        }
        Cmd::Opened => {
            let target = Target::resolve(&cli.on)?;
            let Some(instance) = &target.instance else {
                return Err(fail("unsupported", "only hidden instances log what they open"));
            };
            let text = fs::read_to_string(instance.dir.join("opened.log")).unwrap_or_default();
            let lines: Vec<&str> = text.lines().collect();
            out.done(json!(lines), text.trim_end());
            Ok(())
        }

        Cmd::Status => {
            out.value(&Target::resolve(&cli.on)?.app(json!({"status": true}))?);
            Ok(())
        }
        Cmd::Ls => {
            out.value(&Target::resolve(&cli.on)?.app(json!({"ls": true}))?);
            Ok(())
        }
        Cmd::Do { command, args } => {
            let mut object = serde_json::Map::new();
            for arg in args {
                let (key, value) = arg
                    .split_once('=')
                    .ok_or_else(|| fail("usage", format!("{arg:?} is not key=value")))?;
                let value = serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.into()));
                object.insert(key.into(), value);
            }
            out.value(&Target::resolve(&cli.on)?.app(json!({"do": command, "args": object}))?);
            Ok(())
        }
        Cmd::Commands => {
            let target = Target::resolve(&cli.on)?;
            let output = target.command(target.bin()?).arg("commands").output()?;
            print!("{}", String::from_utf8_lossy(&output.stdout));
            Ok(())
        }
        Cmd::Read { terminal } => hc(&cli, &["read", terminal]),
        Cmd::Send { terminal, text } => {
            guard(&cli, &Target::resolve(&cli.on)?)?;
            hc(&cli, &["send", terminal, text])
        }

        Cmd::Snapshot(args) => {
            let mut session = Session::open(&cli)?;
            let tree = session.tree()?;
            let scope = match &args.within {
                Some(selector) => Some(find_one(&tree, &mut session.refs, selector)?.key),
                None => None,
            };
            let options = Options {
                interactive: args.interactive,
                all: args.all,
                depth: args.depth,
                scope,
            };
            let snapshot = select::snapshot(&tree, &mut session.refs, &options)?;
            if out.json {
                out.value(&serde_json::to_value(&snapshot)?);
            } else {
                print!("{}", snapshot::render(&snapshot, args.source));
            }
            Ok(())
        }
        Cmd::Find { selector } => {
            let mut session = Session::open(&cli)?;
            let tree = session.tree()?;
            let found = find_all(&tree, &mut session.refs, selector)?;
            if out.json {
                out.value(&serde_json::to_value(&found)?);
            } else {
                for entry in &found {
                    println!("{}", describe(entry));
                }
            }
            if found.is_empty() {
                return Err(fail("not_found", format!("nothing visible matches {selector:?}")));
            }
            Ok(())
        }
        Cmd::Get { selector } => {
            let mut session = Session::open(&cli)?;
            let tree = session.tree()?;
            let entry = find_one(&tree, &mut session.refs, selector)?;
            let frame = Frame::new(&tree);
            let node = &tree.nodes[entry.node];
            let value = json!({
                "ref": entry.reference,
                "role": entry.role,
                "name": entry.name,
                "states": entry.states,
                "context": node.key_context,
                "id": node.id,
                "type": node.type_name,
                "source": node.source,
                "bounds": node.bounds,
                "visible": node.visible,
                "value": node.value,
                "checked": node.checked,
                "disabled": node.disabled,
                "focus": node.focus,
                "flags": node.flags,
                "text": frame.text_below(entry.node, false),
            });
            out.value(&value);
            Ok(())
        }
        Cmd::Screenshot {
            output,
            element,
            annotate,
        } => screenshot(&out, &cli, output.clone(), element.as_deref(), *annotate),

        Cmd::Click {
            target,
            button,
            double,
            with,
            at,
        } => {
            let mut session = input_session(&cli)?;
            let ((x, y), what) = session.point(target, *at)?;
            let handled = session.send(click_events(x, y, (*button).into(), *double, modifiers(with)?))?;
            out.done(json!({"clicked": what, "x": x, "y": y, "handled": handled}), format!("clicked {what}"));
            Ok(())
        }
        Cmd::Hover { target, at } => {
            let mut session = input_session(&cli)?;
            let ((x, y), what) = session.point(target, *at)?;
            session.send(vec![InputEvent::MouseMove {
                x,
                y,
                pressed: None,
                modifiers: Modifiers::default(),
            }])?;
            out.done(json!({"hovered": what, "x": x, "y": y}), format!("hovering {what}"));
            Ok(())
        }
        Cmd::Drag {
            from,
            to,
            button,
            with,
            steps,
            from_at,
            to_at,
        } => {
            let mut session = input_session(&cli)?;
            let ((x0, y0), from_what) = session.point(from, *from_at)?;
            let ((x1, y1), to_what) = match parse_pair(to) {
                Some(point) => (point, format!("({}, {})", point.0, point.1)),
                None => {
                    let tree = session.tree()?;
                    let entry = find_one(&tree, &mut session.refs, to)?;
                    let point = match to_at {
                        Some((fx, fy)) => entry.visible.at(*fx, *fy),
                        None => entry.visible.center(),
                    };
                    (point, describe(&entry))
                }
            };
            let button: Button = (*button).into();
            let modifiers = modifiers(with)?;
            let mut events = vec![
                InputEvent::MouseMove {
                    x: x0,
                    y: y0,
                    pressed: None,
                    modifiers,
                },
                InputEvent::MouseDown {
                    x: x0,
                    y: y0,
                    button,
                    click_count: 1,
                    modifiers,
                },
            ];
            let steps = (*steps).max(1);
            for step in 1..=steps {
                let t = step as f32 / steps as f32;
                events.push(InputEvent::MouseMove {
                    x: x0 + (x1 - x0) * t,
                    y: y0 + (y1 - y0) * t,
                    pressed: Some(button),
                    modifiers,
                });
            }
            events.push(InputEvent::MouseUp {
                x: x1,
                y: y1,
                button,
                click_count: 1,
                modifiers,
            });
            session.send(events)?;
            out.done(
                json!({"from": from_what, "to": to_what}),
                format!("dragged {from_what} to {to_what}"),
            );
            Ok(())
        }
        Cmd::Drop { target, files } => {
            let mut session = input_session(&cli)?;
            let ((x, y), what) = session.point(target, None)?;
            let paths: Vec<String> = files
                .iter()
                .map(|f| fs::canonicalize(f).map(|p| p.display().to_string()))
                .collect::<std::io::Result<_>>()
                .context("every dropped file must exist")?;
            let stage = |stage| InputEvent::FileDrop {
                stage,
                x,
                y,
                paths: paths.clone(),
            };
            let handled = session.send(vec![stage(DropStage::Enter), stage(DropStage::Move), stage(DropStage::Submit)])?;
            out.done(json!({"dropped": paths, "on": what, "handled": handled}), format!("dropped {} files on {what}", paths.len()));
            Ok(())
        }
        Cmd::Scroll {
            direction,
            amount,
            within,
            pixels,
            with,
        } => {
            let mut session = input_session(&cli)?;
            let (x, y) = match within {
                Some(selector) => session.point(selector, None)?.0,
                None => {
                    let tree = session.tree()?;
                    (tree.width / 2., tree.height / 2.)
                }
            };
            let (dx, dy) = match direction {
                Direction::Up => (0., *amount),
                Direction::Down => (0., -*amount),
                Direction::Left => (*amount, 0.),
                Direction::Right => (-*amount, 0.),
            };
            session.send(vec![
                InputEvent::MouseMove {
                    x,
                    y,
                    pressed: None,
                    modifiers: Modifiers::default(),
                },
                InputEvent::Scroll {
                    x,
                    y,
                    dx,
                    dy,
                    lines: !*pixels,
                    modifiers: modifiers(with)?,
                },
            ])?;
            out.done(json!({"x": x, "y": y, "dx": dx, "dy": dy}), "scrolled");
            Ok(())
        }
        Cmd::Type { text, into, keys } => {
            let mut session = input_session(&cli)?;
            if let Some(selector) = into {
                let ((x, y), _) = session.point(selector, None)?;
                session.send(click_events(x, y, Button::Left, false, Modifiers::default()))?;
            }
            let handled = session.send(vec![InputEvent::Text {
                text: text.clone(),
                keys: *keys,
            }])?;
            out.done(json!({"typed": text, "handled": handled}), format!("typed {} characters", text.chars().count()));
            Ok(())
        }
        Cmd::Fill { target, text } => {
            let mut session = input_session(&cli)?;
            let ((x, y), what) = session.point(target, None)?;
            session.send(click_events(x, y, Button::Left, false, Modifiers::default()))?;
            session.send(vec![InputEvent::ReplaceText { text: text.clone() }])?;
            out.done(json!({"filled": what, "text": text}), format!("filled {what}"));
            Ok(())
        }
        Cmd::Press { keys } => {
            let mut session = input_session(&cli)?;
            let events: Vec<InputEvent> = keys.iter().map(|k| InputEvent::Key { keystroke: keystroke(k) }).collect();
            let handled = session.send(events)?;
            out.done(json!({"pressed": keys, "handled": handled}), format!("pressed {}", keys.join(" ")));
            Ok(())
        }
        Cmd::Input { file } => raw_input(&out, &cli, file),

        Cmd::Pick { paths } => {
            let mut session = input_session(&cli)?;
            let paths: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
            let _: Value = session.ctrl.call(&Request::Pick { paths: paths.clone() })?;
            out.done(
                json!({"queued": paths}),
                if paths.is_empty() { "the next file panel will be cancelled".into() } else { format!("queued {} paths for the next file panel", paths.len()) },
            );
            Ok(())
        }
        Cmd::Picks => {
            let mut session = Session::open(&cli)?;
            let picks: Vec<PickRecord> = session.ctrl.call(&Request::Picks)?;
            out.value(&serde_json::to_value(picks)?);
            Ok(())
        }
        Cmd::Clipboard { action } => match action {
            ClipboardCmd::Set { text, image } => {
                let mut session = input_session(&cli)?;
                let png = image
                    .as_ref()
                    .map(|p| fs::canonicalize(p).map(|p| p.display().to_string()))
                    .transpose()?;
                let reply: Value = session.ctrl.call(&Request::ClipboardSet { text: text.clone(), png })?;
                out.done(reply, "clipboard set");
                Ok(())
            }
            ClipboardCmd::Get { image } => {
                let mut session = Session::open(&cli)?;
                let png = image.as_ref().map(|p| absolute(p).display().to_string());
                let reply: Value = session.ctrl.call(&Request::ClipboardGet { png })?;
                out.value(&reply);
                Ok(())
            }
        },
        Cmd::Wait {
            selector,
            text,
            gone,
            timeout,
        } => wait(&out, &cli, selector.as_deref(), text.as_deref(), *gone, timeout),
        Cmd::Expect {
            selector,
            gone,
            text,
            state,
            count,
        } => {
            let mut session = Session::open(&cli)?;
            let tree = session.tree()?;
            let found = find_all(&tree, &mut session.refs, selector)?;
            let frame = Frame::new(&tree);
            let problem = if *gone {
                (!found.is_empty()).then(|| format!("{selector:?} is still there: {}", describe(&found[0])))
            } else if let Some(count) = count {
                (found.len() != *count).then(|| format!("{selector:?} matches {} elements, expected {count}", found.len()))
            } else if found.is_empty() {
                Some(format!("nothing visible matches {selector:?}"))
            } else if let Some(text) = text {
                let held = format!("{} {}", found[0].name, frame.text_below(found[0].node, false));
                (!held.contains(text.as_str())).then(|| format!("{} does not contain {text:?}; it reads {held:?}", describe(&found[0])))
            } else if let Some(state) = state {
                (!found[0].states.contains(state)).then(|| format!("{} is not {state}; states: {:?}", describe(&found[0]), found[0].states))
            } else {
                None
            };
            match problem {
                Some(message) => Err(fail("assertion", message)),
                None => {
                    out.done(json!({"ok": true, "matches": found.len()}), "ok");
                    Ok(())
                }
            }
        }
        Cmd::Record { action } => record(&out, &cli, action),
        Cmd::Scratch { action } => scratch(&out, &cli, action),
        Cmd::Mod { action } => mods(&out, &cli, action),
        Cmd::Capabilities => {
            out.value(&json!({
                "protocol": herdr_ctrl_protocol::PROTOCOL_VERSION,
                "targets": ["live", "hidden instances on Xvfb (Linux) or concealed (macOS)"],
                "supported": [
                    "spawn", "instances", "inspect", "stop", "gc", "restart", "exec", "logs", "opened",
                    "status", "ls", "do", "commands", "read", "send",
                    "snapshot", "find", "get", "screenshot",
                    "click", "hover", "drag", "drop", "scroll", "type", "fill", "press", "input",
                    "pick", "picks", "clipboard", "wait", "expect", "record", "scratch", "mod", "mcp"
                ],
                "selectors": ["@eN", "#id", "id=", "text=", "ctx=", "src=", "<role> \"name\"", "nth=N", "plain text", "a >> b"],
                "unsupported": {
                    "computed styles and event handlers": "the frame record holds bounds, ids, roles, text and handler kinds only",
                    "frame timing and traces": "not recorded",
                    "IME composition": "text is inserted whole",
                    "accessibility tree": "Herdr sets no roles; the tree is inferred from handlers",
                    "several windows or a shared core between instances": "each instance has one window and its own core",
                    "mod source, spec, console and eval": "use `herdr-ctrl logs`, `mod check` and `status` errors",
                    "mod= and pane= selectors": "the tree does not say which mod or pane built an element; use ctx=, #id or text",
                    "live window logs": "hidden instances only",
                    "restarting the live core": "`restart` on the live window keeps its core and terminals"
                }
            }));
            Ok(())
        }
        Cmd::Skill => {
            print!("{SKILL}");
            Ok(())
        }
        Cmd::Mcp => mcp::serve(),
        Cmd::Supervise { name } => instance::supervise(name),
    }
}

fn restart_live(out: &Out, target: &Target) -> Result<()> {
    let previous = target.ctrl()?.hello.pid;
    if let Err(error) = target.app(json!({"do": "app.restart", "args": {}}))
        && is_kind(&error, "app")
    {
        return Err(Fail::new("unsupported", format!("the live window refused app.restart: {error}"))
            .hint("this Herdr build predates app.restart (fork personal-v0.1.13.37); quit and reopen Herdr yourself")
            .into());
    }
    let socket = target.home.join(herdr_ctrl_protocol::SOCKET_NAME);
    let start = Instant::now();
    while instance::alive(Some(previous)) {
        if start.elapsed() > Duration::from_secs(30) {
            return Err(fail("timeout", format!("the live window (pid {previous}) did not quit within 30s")));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    loop {
        if let Ok(mut ctrl) = Ctrl::connect(&socket)
            && ctrl.hello.pid != previous
            && let Ok(window) = ctrl.window()
        {
            out.done(
                json!({"name": target::LIVE, "app_pid": ctrl.hello.pid, "previous_pid": previous, "window": window.window}),
                format!("restarted live (pid {previous} -> {})", ctrl.hello.pid),
            );
            return Ok(());
        }
        if start.elapsed() > Duration::from_secs(90) {
            return Err(Fail::new("timeout", "the live window quit but no new window answered within 90s")
                .hint("start Herdr yourself; your terminals kept running in herdr-core")
                .into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map(|d| d.join(path)).unwrap_or_else(|_| path.to_path_buf())
    }
}

fn instance_summary(instance: &Instance) -> Value {
    json!({
        "name": instance.name,
        "state": instance.state,
        "display": instance.display,
        "app_pid": instance.app_pid,
        "alive": instance::alive(instance.app_pid),
        "supervised": instance.supervised(),
        "bin": instance.bin,
        "home": instance.home(),
        "log": instance.log_path(),
        "generation": instance.generation,
        "expires_in": instance.expires_at().map(|at| at.saturating_sub(instance::now())),
    })
}

fn spawn(out: &Out, cli: &Cli, args: &SpawnArgs) -> Result<()> {
    let bin = match (&args.bin, &args.build) {
        (Some(bin), _) => fs::canonicalize(bin).with_context(|| format!("{} does not exist", bin.display()))?,
        (None, Some(checkout)) => {
            let mut cargo = std::process::Command::new("cargo");
            cargo.arg("build").current_dir(checkout).stdout(Stdio::null());
            if args.release {
                cargo.arg("--release");
            }
            if !cargo.status()?.success() {
                return Err(fail("build", format!("cargo build failed in {}", checkout.display())));
            }
            fs::canonicalize(checkout.join("target").join(if args.release { "release" } else { "debug" }).join("herdr-app"))?
        }
        (None, None) => instance::live_bin()?,
    };
    let name = args.name.clone().or_else(|| (cli.agent != "agent").then(|| cli.agent.clone()).filter(|n| !state_root().join("instances").join(n).exists()));
    let (instance, mut ctrl) = instance::spawn(SpawnOptions {
        name,
        bin,
        fresh: args.fresh,
        mods: args.mods.iter().map(|m| absolute(m)).collect(),
        width: args.size.0,
        height: args.size.1,
        ttl_secs: instance::parse_duration(&args.ttl)?,
        env: args.env.clone(),
        log: args.log.clone(),
    })?;
    let window = ctrl.window()?;
    out.done(
        json!({
            "name": instance.name,
            "display": instance.display,
            "app_pid": instance.app_pid,
            "home": instance.home(),
            "bin": instance.bin,
            "window": {"width": window.width, "height": window.height, "scale": window.scale},
            "ttl_secs": instance.ttl_secs,
        }),
        format!(
            "spawned {} on {} ({}x{}); drive it with --on {} or HERDR_CTRL_ON={}",
            instance.name,
            instance.display.as_deref().unwrap_or("a concealed window"),
            window.width,
            window.height,
            instance.name,
            instance.name
        ),
    );
    Ok(())
}

fn hc(cli: &Cli, args: &[&str]) -> Result<()> {
    let target = Target::resolve(&cli.on)?;
    let status = target.command("hc").args(args).status()?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

fn screenshot(out: &Out, cli: &Cli, output: Option<PathBuf>, element: Option<&str>, annotate: bool) -> Result<()> {
    let mut session = Session::open(cli)?;
    let path = match output {
        Some(path) => absolute(&path),
        None => {
            let dir = state_root().join("shots");
            fs::create_dir_all(&dir)?;
            dir.join(format!("{}-{}.png", session.target.name, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis()))
        }
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let info: ScreenshotInfo = session.ctrl.call(&Request::Screenshot {
        window: session.window,
        path: path.display().to_string(),
    })?;
    let tree = session.tree()?;
    let scale = if tree.width > 0. { info.width as f32 / tree.width } else { 1. };
    let mut legend = Vec::new();
    if element.is_some() || annotate {
        let mut image = image::open(&path)?.to_rgba8();
        if annotate {
            let snapshot = select::snapshot(&tree, &mut session.refs, &Options { interactive: true, ..Default::default() })?;
            legend = annotate::annotate(&mut image, &snapshot);
        }
        if let Some(selector) = element {
            let entry = find_one(&tree, &mut session.refs, selector)?;
            let v = entry.visible;
            let x = (v.x * scale).max(0.) as u32;
            let y = (v.y * scale).max(0.) as u32;
            let w = ((v.w * scale) as u32).min(image.width().saturating_sub(x)).max(1);
            let h = ((v.h * scale) as u32).min(image.height().saturating_sub(y)).max(1);
            image = image::imageops::crop_imm(&image, x, y, w, h).to_image();
        }
        image.save(&path)?;
    }
    let (width, height) = image::image_dimensions(&path)?;
    let value = json!({"path": path, "width": width, "height": height, "scale": scale, "legend": legend});
    out.done(value, format!("{} ({width}x{height}, scale {scale})", path.display()));
    Ok(())
}

fn frame_hash(tree: &RawTree) -> u64 {
    let mut hasher = DefaultHasher::new();
    serde_json::to_string(&(&tree.nodes, &tree.texts)).unwrap_or_default().hash(&mut hasher);
    hasher.finish()
}

fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn visible_text(tree: &RawTree) -> String {
    let runs: Vec<&str> = tree.texts.iter().filter(|t| !t.visible.is_empty()).map(|t| t.text.as_str()).collect();
    squash(&runs.join(" "))
}

fn wait(out: &Out, cli: &Cli, selector: Option<&str>, text: Option<&str>, gone: bool, timeout: &str) -> Result<()> {
    let timeout = Duration::from_secs(instance::parse_duration(timeout)?.unwrap_or(3600));
    let mut session = Session::open(cli)?;
    let start = Instant::now();
    let mut last_hash = None;
    let mut stable_since = Instant::now();
    loop {
        let tree = session.tree()?;
        let done = match (selector, text) {
            (Some(selector), _) => {
                let found = match find_all(&tree, &mut session.refs, selector) {
                    Ok(found) => found,
                    Err(e) if is_kind(&e, "stale_ref") || is_kind(&e, "not_visible") => Vec::new(),
                    Err(e) => return Err(e),
                };
                let matched = match text {
                    Some(text) => found.iter().any(|e| e.name.contains(text) || Frame::new(&tree).text_below(e.node, false).contains(text)),
                    None => !found.is_empty(),
                };
                matched != gone
            }
            (None, Some(text)) => {
                let present = visible_text(&tree).contains(&squash(text));
                present != gone
            }
            (None, None) => {
                let hash = frame_hash(&tree);
                if last_hash != Some(hash) {
                    last_hash = Some(hash);
                    stable_since = Instant::now();
                }
                stable_since.elapsed() >= Duration::from_millis(300)
            }
        };
        if done {
            let waited = start.elapsed().as_millis();
            out.done(json!({"ok": true, "waited_ms": waited}), format!("ready after {waited}ms"));
            return Ok(());
        }
        if start.elapsed() > timeout {
            let hint = if selector.is_none() && text.is_none() {
                "something keeps redrawing, such as a spinner or a busy terminal; wait for a specific element or text instead"
            } else {
                "take a snapshot to see what is on screen"
            };
            return Err(Fail::new("timeout", format!("still waiting after {}s", timeout.as_secs())).hint(hint).into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn raw_input(out: &Out, cli: &Cli, file: &str) -> Result<()> {
    let text = if file == "-" {
        std::io::read_to_string(std::io::stdin())?
    } else {
        fs::read_to_string(file)?
    };
    let steps: Vec<Value> = serde_json::from_str(&text).map_err(|e| fail("usage", format!("the input file is not a JSON list: {e}")))?;
    let mut session = input_session(cli)?;
    let mut held_buttons: Vec<(Button, f32, f32)> = Vec::new();
    let mut held_keys: Vec<String> = Vec::new();
    let mut receipts = Vec::new();
    let mut batch: Vec<InputEvent> = Vec::new();
    let result = (|| -> Result<()> {
        for step in &steps {
            if step["type"] == "wait" {
                if !batch.is_empty() {
                    receipts.extend(session.send(std::mem::take(&mut batch))?);
                }
                std::thread::sleep(Duration::from_millis(step["ms"].as_u64().unwrap_or(100)));
                continue;
            }
            let event: InputEvent = serde_json::from_value(step.clone()).map_err(|e| fail("usage", format!("bad step {step}: {e}")))?;
            match &event {
                InputEvent::MouseDown { x, y, button, .. } => held_buttons.push((*button, *x, *y)),
                InputEvent::MouseUp { button, .. } => held_buttons.retain(|(b, _, _)| b != button),
                InputEvent::KeyDown { keystroke, .. } => held_keys.push(keystroke.clone()),
                InputEvent::KeyUp { keystroke } => held_keys.retain(|k| k != keystroke),
                _ => {}
            }
            batch.push(event);
        }
        if !batch.is_empty() {
            receipts.extend(session.send(std::mem::take(&mut batch))?);
        }
        Ok(())
    })();
    if let Err(error) = result {
        let mut release: Vec<InputEvent> = held_buttons
            .iter()
            .map(|(button, x, y)| InputEvent::MouseUp {
                x: *x,
                y: *y,
                button: *button,
                click_count: 1,
                modifiers: Modifiers::default(),
            })
            .collect();
        release.extend(held_keys.iter().map(|k| InputEvent::KeyUp { keystroke: k.clone() }));
        release.push(InputEvent::Modifiers { modifiers: Modifiers::default() });
        let _ = session.send(release);
        return Err(error);
    }
    out.done(json!({"handled": receipts}), format!("sent {} events", receipts.len()));
    Ok(())
}

fn record(out: &Out, cli: &Cli, action: &RecordCmd) -> Result<()> {
    let mut session = Session::open(cli)?;
    match action {
        RecordCmd::Start { fps } => {
            let dir = state_root().join("recordings").join(format!(
                "{}-{}",
                session.target.name,
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis()
            ));
            fs::create_dir_all(&dir)?;
            let _: Value = session.ctrl.call(&Request::RecordStart {
                window: session.window,
                dir: dir.display().to_string(),
                fps: *fps,
            })?;
            out.done(json!({"dir": dir}), format!("recording into {}", dir.display()));
            Ok(())
        }
        RecordCmd::Stop { output } => {
            let recording: Recording = session.ctrl.call(&Request::RecordStop)?;
            let output = output.as_ref().map(|o| absolute(o)).unwrap_or_else(|| PathBuf::from(format!("{}.mp4", recording.dir)));
            let pattern = format!("{}/frame-%05d.png", recording.dir);
            let rate = recording.fps.to_string();
            let mut ffmpeg = std::process::Command::new("ffmpeg");
            ffmpeg.args(["-y", "-loglevel", "error", "-framerate", &rate, "-i", &pattern]);
            if output.extension().is_some_and(|e| e == "gif") {
                ffmpeg.args(["-vf", "split[a][b];[a]palettegen[p];[b][p]paletteuse"]);
            } else {
                ffmpeg.args(["-vf", "pad=ceil(iw/2)*2:ceil(ih/2)*2", "-pix_fmt", "yuv420p"]);
            }
            let encoded = ffmpeg.arg(&output).status().map(|s| s.success()).unwrap_or(false);
            if !encoded {
                return Err(Fail::new("failed", format!("ffmpeg could not encode the {} frames", recording.frames))
                    .hint(format!("the frames are in {}", recording.dir))
                    .into());
            }
            let _ = fs::remove_dir_all(&recording.dir);
            out.done(
                json!({"path": output, "frames": recording.frames, "seconds": recording.seconds}),
                format!("{} ({} frames, {:.1}s)", output.display(), recording.frames, recording.seconds),
            );
            Ok(())
        }
    }
}

fn scratch(out: &Out, cli: &Cli, action: &ScratchCmd) -> Result<()> {
    let target = Target::resolve(&cli.on)?;
    let path = scratch_path(cli);
    let saved: Option<Scratch> = fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok());
    match action {
        ScratchCmd::Open => {
            let previous = current_space(&target)?;
            if let Some(saved) = &saved
                && target.app(json!({"do": "space.select", "args": {"id": saved.space}})).is_ok()
            {
                out.done(json!({"space": saved.space}), format!("selected your scratch space {}", saved.space));
                return Ok(());
            }
            target.app(json!({"do": "space.new", "args": {"name": format!("ctrl-{}", cli.agent)}}))?;
            let space = current_space(&target)?.context("Herdr reports no space after creating one")?;
            fs::create_dir_all(path.parent().unwrap_or(Path::new("/")))?;
            fs::write(&path, serde_json::to_string(&Scratch { space: space.clone(), previous })?)?;
            out.done(json!({"space": space}), format!("opened scratch space {space}; input is allowed while it is selected"));
            Ok(())
        }
        ScratchCmd::Close => {
            let Some(saved) = saved else {
                return Err(fail("not_found", "you have no scratch space open"));
            };
            let was_selected = current_space(&target)?.as_deref() == Some(saved.space.as_str());
            let _ = target.app(json!({"do": "space.close", "args": {"id": saved.space, "confirmed": true}}));
            if was_selected && let Some(previous) = &saved.previous {
                let _ = target.app(json!({"do": "space.select", "args": {"id": previous}}));
            }
            fs::remove_file(&path)?;
            out.done(json!({"closed": saved.space}), "closed your scratch space");
            Ok(())
        }
    }
}

fn backup_dir(target: &Target, name: &str) -> PathBuf {
    if let Some(instance) = &target.instance {
        return instance.dir.join("backup").join(name);
    }
    let cache = std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from).unwrap_or_else(|| home_dir().join(".cache"));
    cache.join("herdr-ctrl").join("backup").join(&target.name).join(name)
}

fn mods(out: &Out, cli: &Cli, action: &ModCmd) -> Result<()> {
    let target = Target::resolve(&cli.on)?;
    let mods_dir = target.home.join("mods");
    match action {
        ModCmd::Ls => {
            let mut names: Vec<String> = fs::read_dir(&mods_dir)?
                .flatten()
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            out.done(json!(names), names.join("\n"));
            Ok(())
        }
        ModCmd::Check { dir } => {
            let output = target.command(target.bin()?).arg("check").arg(absolute(dir)).output()?;
            print!("{}", String::from_utf8_lossy(&output.stdout));
            eprint!("{}", String::from_utf8_lossy(&output.stderr));
            if !output.status.success() {
                return Err(fail("assertion", "the mod failed its check"));
            }
            Ok(())
        }
        ModCmd::Load { dir, name } => {
            let source = fs::canonicalize(dir).with_context(|| format!("{} does not exist", dir.display()))?;
            let name = match name {
                Some(name) => name.clone(),
                None => source.file_name().context("a mod folder needs a name")?.to_string_lossy().into_owned(),
            };
            let installed = mods_dir.join(&name);
            if source == fs::canonicalize(&installed).unwrap_or_default() {
                return Err(fail("usage", "that folder is the installed mod itself"));
            }
            let backup = backup_dir(&target, &name);
            if installed.exists() && !backup.exists() {
                fs::create_dir_all(&backup)?;
                instance::copy_tree(&installed, &backup)?;
            }
            let _ = fs::remove_dir_all(&installed);
            instance::copy_tree(&source, &installed)?;
            let restore_hint = if backup.exists() {
                format!("; restore the previous copy with `herdr-ctrl mod restore {name}`")
            } else {
                String::new()
            };
            out.done(
                json!({"loaded": name, "into": installed, "backup": backup.exists().then_some(&backup)}),
                format!("loaded {name}{restore_hint}"),
            );
            Ok(())
        }
        ModCmd::Restore { name } => {
            let backup = backup_dir(&target, name);
            if !backup.exists() {
                return Err(fail("not_found", format!("no backup of {name}")));
            }
            let installed = mods_dir.join(name);
            let _ = fs::remove_dir_all(&installed);
            instance::copy_tree(&backup, &installed)?;
            fs::remove_dir_all(&backup)?;
            out.done(json!({"restored": name}), format!("restored {name}"));
            Ok(())
        }
    }
}
