//! Hidden Herdr instances: each one runs with its own home, state and core, on its own Xvfb display
//! on Linux and as a concealed, never-activated app on macOS, owned by a detached supervisor that
//! tears everything down when the lease runs out.

use crate::fail::{Fail, fail};
use crate::target::{Ctrl, home_dir, live_home};
use anyhow::{Context, Result};
use herdr_ctrl_protocol::SOCKET_NAME;
use serde::{Deserialize, Serialize};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SKIPPED_FROM_LIVE: &[&str] = &[
    "media-viewer-cache",
    "frame",
    "window.json",
    "windows.json",
    "view.json",
    "installed.json",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instance {
    pub name: String,
    pub dir: PathBuf,
    pub bin: PathBuf,
    pub width: u32,
    pub height: u32,
    pub ttl_secs: Option<u64>,
    pub created: u64,
    pub state: String,
    #[serde(default)]
    pub display: Option<String>,
    #[serde(default)]
    pub supervisor_pid: Option<u32>,
    #[serde(default)]
    pub xvfb_pid: Option<u32>,
    #[serde(default)]
    pub app_pid: Option<u32>,
    #[serde(default)]
    pub generation: u32,
    pub env: Vec<(String, String)>,
    #[serde(default)]
    pub error: Option<String>,
}

pub struct SpawnOptions {
    pub name: Option<String>,
    pub bin: PathBuf,
    pub fresh: bool,
    pub mods: Vec<PathBuf>,
    pub width: u32,
    pub height: u32,
    pub ttl_secs: Option<u64>,
    pub env: Vec<(String, String)>,
    pub log: Option<String>,
}

pub fn state_root() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(runtime) => PathBuf::from(runtime).join("herdr-ctrl"),
        None => PathBuf::from(format!("/tmp/herdr-ctrl-{}", unsafe { libc::getuid() })),
    }
}

fn instances_dir() -> PathBuf {
    state_root().join("instances")
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn alive(pid: Option<u32>) -> bool {
    pid.is_some_and(|pid| unsafe { libc::kill(pid as i32, 0) } == 0)
}

pub fn live_bin() -> Result<PathBuf> {
    if let Some(bin) = std::env::var_os("HERDR_CTRL_BIN") {
        return Ok(bin.into());
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join("herdr-app"))
        .find(|candidate| candidate.is_file())
        .map(|bin| fs::canonicalize(&bin).unwrap_or(bin))
        .ok_or_else(|| fail("not_found", "herdr-app is not on PATH; pass --bin or set HERDR_CTRL_BIN"))
}

pub fn clean_env(command: &mut Command) {
    for (key, _) in std::env::vars_os() {
        let key = key.to_string_lossy();
        if key.starts_with("HERDR_") || key.starts_with("HC_") || key == "WAYLAND_DISPLAY" || key == "DISPLAY" {
            command.env_remove(key.as_ref());
        }
    }
}

impl Instance {
    pub fn load(name: &str) -> Result<Instance> {
        let path = instances_dir().join(name).join("instance.json");
        let text = fs::read_to_string(&path).map_err(|_| {
            let known: Vec<String> = list().into_iter().map(|i| i.name).collect();
            Fail::new("no_instance", format!("no hidden instance named {name:?}")).hint(if known.is_empty() {
                "start one with `herdr-ctrl spawn`".to_string()
            } else {
                format!("running: {}", known.join(", "))
            })
        })?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn save(&self) -> Result<()> {
        let path = self.dir.join("instance.json");
        let partial = self.dir.join("instance.json.part");
        fs::write(&partial, serde_json::to_string_pretty(self)?)?;
        fs::rename(partial, path)?;
        Ok(())
    }

    pub fn home(&self) -> PathBuf {
        self.dir.join("home")
    }

    pub fn log_path(&self) -> PathBuf {
        self.dir.join("app.log")
    }

    pub fn renew(&self) -> Result<()> {
        if let Some(ttl) = self.ttl_secs {
            fs::write(self.dir.join("lease"), (now() + ttl).to_string())?;
        }
        Ok(())
    }

    pub fn expires_at(&self) -> Option<u64> {
        fs::read_to_string(self.dir.join("lease")).ok()?.trim().parse().ok()
    }

    pub fn supervised(&self) -> bool {
        alive(self.supervisor_pid)
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.bin);
        clean_env(&mut command);
        command.envs(self.env.iter().map(|(k, v)| (k, v)));
        command
    }

    pub fn launch_app(&mut self) -> Result<()> {
        let log = fs::OpenOptions::new().create(true).append(true).open(self.log_path())?;
        let mut command = self.command();
        command
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .current_dir(home_dir());
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let child = command.spawn().with_context(|| format!("could not start {}", self.bin.display()))?;
        self.app_pid = Some(child.id());
        self.generation += 1;
        self.save()
    }

    pub fn stop_app(&mut self) {
        if let Some(pid) = self.app_pid.take() {
            terminate(pid);
        }
    }

    pub fn stop_core(&self) {
        let mut command = Command::new("hc");
        clean_env(&mut command);
        command.envs(self.env.iter().map(|(k, v)| (k, v)));
        let _ = command
            .args(["server", "stop"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    pub fn wait_ready(&self, timeout: Duration) -> Result<Ctrl> {
        let start = Instant::now();
        let ctrl_socket = self.home().join(SOCKET_NAME);
        let app_socket = self.home().join("herdr-app.sock");
        let mut app_up: Option<Instant> = None;
        loop {
            if let Ok(fresh) = Instance::load(&self.name) {
                if fresh.state == "failed" {
                    return Err(fail("failed", fresh.error.unwrap_or_else(|| "the instance failed to start".into())));
                }
                if !alive(fresh.app_pid) && fresh.app_pid.is_some() && start.elapsed() > Duration::from_secs(1) {
                    return Err(Fail::new("app_crash", "herdr-app exited during startup")
                        .hint(format!("see {}", self.log_path().display()))
                        .into());
                }
            }
            if let Ok(mut ctrl) = Ctrl::connect(&ctrl_socket)
                && ctrl.window().is_ok()
            {
                return Ok(ctrl);
            }
            if app_socket.exists() && app_up.get_or_insert_with(Instant::now).elapsed() > Duration::from_secs(15) {
                return Err(Fail::new("no_ctrl", "the window started, but it has no herdr-ctrl server")
                    .hint("this binary was built without the ctrl feature; pass --bin or --build with the JosXa/herdr-app fork")
                    .into());
            }
            if start.elapsed() > timeout {
                return Err(Fail::new("timeout", "the instance did not open a window in time")
                    .hint(format!("see {}", self.log_path().display()))
                    .into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

pub fn list() -> Vec<Instance> {
    let Ok(entries) = fs::read_dir(instances_dir()) else {
        return Vec::new();
    };
    let mut instances: Vec<Instance> = entries
        .flatten()
        .filter_map(|e| fs::read_to_string(e.path().join("instance.json")).ok())
        .filter_map(|text| serde_json::from_str(&text).ok())
        .collect();
    instances.sort_by(|a, b| a.name.cmp(&b.name));
    instances
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 32 && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

const CORE_SESSION: &str = "ctrl";
const LONGEST_SOCKET: &str = "state/herdr-core/ctrl/ctrl.holder.00000000.sock";
const SOCKET_PATH_MAX: usize = 103;

fn fits_socket_paths(name: &str) -> bool {
    instances_dir().join(name).join(LONGEST_SOCKET).as_os_str().len() <= SOCKET_PATH_MAX
}

fn pick_name(wanted: Option<String>) -> Result<String> {
    if let Some(name) = wanted {
        if !valid_name(&name) || name == crate::target::LIVE {
            return Err(fail("usage", format!("{name:?} is not a usable name: use lowercase letters, digits and dashes")));
        }
        if !fits_socket_paths(&name) {
            return Err(fail("usage", format!("{name:?} is too long: its core socket path would pass the unix limit")));
        }
        if instances_dir().join(&name).exists() {
            return Err(Fail::new("exists", format!("an instance named {name:?} already exists"))
                .hint(format!("use it with --on {name}, or stop it with `herdr-ctrl stop {name}`"))
                .into());
        }
        return Ok(name);
    }
    (1..)
        .map(|n| format!("h{n}"))
        .find(|name| !instances_dir().join(name).exists())
        .context("no free name")
}

pub fn spawn(options: SpawnOptions) -> Result<(Instance, Ctrl)> {
    gc();
    let xvfb = if cfg!(target_os = "macos") { None } else { Some(xvfb_path()?) };
    let name = pick_name(options.name)?;
    let dir = instances_dir().join(&name);
    let home = dir.join("home");
    fs::create_dir_all(&home)?;
    fs::create_dir_all(dir.join("state"))?;
    fs::create_dir_all(dir.join("bin"))?;
    let stub = dir.join(if cfg!(target_os = "macos") { "bin/open" } else { "bin/xdg-open" });
    fs::write(
        &stub,
        format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n", dir.join("opened.log").display()),
    )?;
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755))?;

    if !options.fresh {
        copy_dir(&live_home(), &home, &|name| {
            SKIPPED_FROM_LIVE.contains(&name) || name.ends_with(".sock") || name.starts_with("update-notes")
        })?;
    }
    fs::write(
        home.join("window.json"),
        serde_json::json!({"state": "windowed", "bounds": [0, 0, options.width, options.height]}).to_string(),
    )?;
    for source in &options.mods {
        let mod_name = source.file_name().context("a mod folder needs a name")?;
        let target = home.join("mods").join(mod_name);
        let _ = fs::remove_dir_all(&target);
        copy_dir(source, &target, &|_| false)?;
    }

    let path = std::env::var("PATH").unwrap_or_default();
    let mut env = vec![
        ("HERDR_APP_HOME".into(), home.display().to_string()),
        ("XDG_STATE_HOME".into(), dir.join("state").display().to_string()),
        ("HC_SESSION".into(), CORE_SESSION.into()),
        ("HERDR_APP_ID".into(), format!("herdr-ctrl-{name}")),
        ("HERDR_APP_NO_SOUND".into(), "1".into()),
        ("HERDR_CTRL_ON".into(), name.clone()),
        ("PATH".into(), format!("{}:{path}", dir.join("bin").display())),
    ];
    if cfg!(target_os = "macos") {
        env.push(("HERDR_CTRL_HIDDEN".into(), "1".into()));
    }
    if let Some(filter) = options.log {
        env.push(("HERDR_APP_LOG".into(), filter));
    }
    env.extend(options.env);

    let instance = Instance {
        name: name.clone(),
        dir: dir.clone(),
        bin: options.bin,
        width: options.width,
        height: options.height,
        ttl_secs: options.ttl_secs,
        created: now(),
        state: "starting".into(),
        display: None,
        supervisor_pid: None,
        xvfb_pid: None,
        app_pid: None,
        generation: 0,
        env,
        error: None,
    };
    instance.save()?;
    instance.renew()?;

    let log = fs::File::create(dir.join("supervisor.log"))?;
    let mut supervisor = Command::new(std::env::current_exe()?);
    supervisor
        .args(["_supervise", &name])
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    if let Some(xvfb) = xvfb {
        supervisor.env("HERDR_CTRL_XVFB", xvfb);
    }
    unsafe {
        supervisor.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    supervisor.spawn().context("could not start the supervisor")?;

    let ctrl = instance.wait_ready(Duration::from_secs(90)).inspect_err(|_| stop(&name, Duration::from_secs(15)))?;
    Ok((Instance::load(&name)?, ctrl))
}

fn xvfb_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("HERDR_CTRL_XVFB") {
        return Ok(path.into());
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join("Xvfb"))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            Fail::new("unsupported", "hidden instances need Xvfb, and it is not on PATH")
                .hint("install it (Arch: xorg-server-xvfb, Debian: xvfb, Nix: xorg.xvfb) or set HERDR_CTRL_XVFB")
                .into()
        })
}

fn free_display() -> Option<u32> {
    (90..400).find(|n| {
        !Path::new(&format!("/tmp/.X11-unix/X{n}")).exists() && !Path::new(&format!("/tmp/.X{n}-lock")).exists()
    })
}

pub fn supervise(name: &str) -> Result<()> {
    let mut instance = Instance::load(name)?;
    instance.supervisor_pid = Some(std::process::id());
    instance.save()?;
    let result = start(&mut instance);
    if let Err(error) = &result {
        instance.state = "failed".into();
        instance.error = Some(format!("{error:#}"));
        instance.save()?;
        std::thread::sleep(Duration::from_secs(2));
        teardown(&mut instance);
        return result;
    }
    loop {
        std::thread::sleep(Duration::from_millis(500));
        unsafe {
            while libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) > 0 {}
        }
        if let Ok(fresh) = Instance::load(name) {
            instance = fresh;
        }
        if instance.dir.join("stop").exists() {
            eprintln!("stop requested");
            break;
        }
        if instance.expires_at().is_some_and(|at| now() >= at) {
            eprintln!("lease expired");
            break;
        }
        if instance.xvfb_pid.is_some() && !alive(instance.xvfb_pid) {
            eprintln!("Xvfb exited");
            break;
        }
    }
    teardown(&mut instance);
    Ok(())
}

fn start(instance: &mut Instance) -> Result<()> {
    if cfg!(target_os = "macos") {
        instance.launch_app()?;
        instance.state = "running".into();
        return instance.save();
    }
    let xvfb = xvfb_path()?;
    let mut last_error = None;
    for _ in 0..5 {
        let display = free_display().context("no free X display between :90 and :399")?;
        let log = fs::File::create(instance.dir.join("xvfb.log"))?;
        let mut child = Command::new(&xvfb)
            .arg(format!(":{display}"))
            .args(["-screen", "0", &format!("{}x{}x24", instance.width, instance.height)])
            .args(["-nolisten", "tcp"])
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .spawn()
            .with_context(|| format!("could not run {}", xvfb.display()))?;
        let socket = PathBuf::from(format!("/tmp/.X11-unix/X{display}"));
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) && !socket.exists() {
            if child.try_wait()?.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        if socket.exists() && child.try_wait()?.is_none() {
            instance.display = Some(format!(":{display}"));
            instance.xvfb_pid = Some(child.id());
            instance.env.retain(|(k, _)| k != "DISPLAY");
            instance.env.push(("DISPLAY".into(), format!(":{display}")));
            instance.launch_app()?;
            instance.state = "running".into();
            return instance.save();
        }
        let _ = child.kill();
        last_error = Some(fs::read_to_string(instance.dir.join("xvfb.log")).unwrap_or_default());
    }
    Err(anyhow::anyhow!("Xvfb did not start: {}", last_error.unwrap_or_default()))
}

fn terminate(pid: u32) {
    let pid = pid as i32;
    unsafe {
        libc::kill(-pid, libc::SIGTERM);
        libc::kill(pid, libc::SIGTERM);
    }
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(3) {
        unsafe {
            libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG);
        }
        if unsafe { libc::kill(pid, 0) } != 0 {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
        libc::kill(pid, libc::SIGKILL);
        libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG);
    }
}

fn teardown(instance: &mut Instance) {
    instance.stop_app();
    instance.stop_core();
    if let Some(pid) = instance.xvfb_pid.take() {
        terminate(pid);
    }
    let _ = fs::remove_dir_all(&instance.dir);
    for kind in ["refs", "scratch"] {
        let Ok(entries) = fs::read_dir(state_root().join(kind)) else {
            continue;
        };
        let prefix = format!("{}.", instance.name);
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(&prefix) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

/// Asks the supervisor to tear the instance down and waits until it has.
pub fn stop(name: &str, timeout: Duration) {
    let Ok(mut instance) = Instance::load(name) else {
        return;
    };
    if !instance.supervised() {
        teardown(&mut instance);
        return;
    }
    let _ = fs::write(instance.dir.join("stop"), "");
    let start = Instant::now();
    while instance.dir.exists() && start.elapsed() < timeout {
        std::thread::sleep(Duration::from_millis(100));
    }
    if instance.dir.exists() {
        if let Some(pid) = instance.supervisor_pid {
            terminate(pid);
        }
        teardown(&mut instance);
    }
}

/// Removes instances whose supervisor is gone, such as after a crash or a reboot.
pub fn gc() -> Vec<String> {
    let mut removed = Vec::new();
    for mut instance in list() {
        let stale_start = instance.supervisor_pid.is_none() && now().saturating_sub(instance.created) > 120;
        if (instance.supervisor_pid.is_some() && !instance.supervised()) || stale_start {
            teardown(&mut instance);
            removed.push(instance.name);
        }
    }
    removed
}

pub fn restart(instance: &mut Instance, keep_core: bool) -> Result<Ctrl> {
    instance.stop_app();
    if !keep_core {
        instance.stop_core();
    }
    let _ = fs::remove_file(instance.home().join(SOCKET_NAME));
    let _ = fs::remove_file(instance.home().join("herdr-app.sock"));
    instance.launch_app()?;
    instance.wait_ready(Duration::from_secs(90))
}

fn copy_dir(from: &Path, to: &Path, skip: &dyn Fn(&str) -> bool) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from).with_context(|| format!("could not read {}", from.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if skip(&name_str) {
            continue;
        }
        let source = entry.path();
        let Ok(meta) = fs::metadata(&source) else {
            continue;
        };
        let target = to.join(&name);
        if meta.is_dir() {
            copy_dir(&source, &target, &|_| false)?;
        } else if meta.is_file() {
            fs::copy(&source, &target)?;
        }
    }
    Ok(())
}

pub fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    copy_dir(from, to, &|_| false)
}

pub fn parse_duration(text: &str) -> Result<Option<u64>> {
    if text == "0" || text == "none" {
        return Ok(None);
    }
    let (number, unit) = text.split_at(text.find(|c: char| !c.is_ascii_digit()).unwrap_or(text.len()));
    let number: u64 = number.parse().map_err(|_| fail("usage", format!("{text:?} is not a duration like 90s, 30m or 2h")))?;
    let seconds = match unit {
        "" | "s" => number,
        "m" => number * 60,
        "h" => number * 3600,
        _ => return Err(fail("usage", format!("{text:?} is not a duration like 90s, 30m or 2h"))),
    };
    Ok(Some(seconds))
}
