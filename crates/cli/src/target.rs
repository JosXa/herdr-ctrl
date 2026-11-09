//! Which Herdr a command talks to, and the two sockets every Herdr home holds.

use crate::fail::{Fail, fail};
use crate::instance::{self, Instance};
use anyhow::{Context, Result};
use herdr_ctrl_protocol::{ErrorKind, Hello, PROTOCOL_VERSION, RawTree, Request, Response, SOCKET_NAME, WindowInfo};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

pub const LIVE: &str = "live";

pub struct Target {
    pub name: String,
    pub home: PathBuf,
    pub instance: Option<Instance>,
}

impl Target {
    pub fn resolve(on: &str) -> Result<Target> {
        if on == LIVE {
            return Ok(Target {
                name: LIVE.into(),
                home: live_home(),
                instance: None,
            });
        }
        let instance = Instance::load(on)?;
        instance.renew()?;
        Ok(Target {
            name: on.into(),
            home: instance.home(),
            instance: Some(instance),
        })
    }

    pub fn is_live(&self) -> bool {
        self.instance.is_none()
    }

    pub fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(program);
        if let Some(instance) = &self.instance {
            instance::clean_env(&mut command);
            command.envs(instance.env.iter().map(|(k, v)| (k, v)));
        }
        command
    }

    pub fn bin(&self) -> Result<PathBuf> {
        match &self.instance {
            Some(instance) => Ok(instance.bin.clone()),
            None => instance::live_bin(),
        }
    }

    pub fn ctrl(&self) -> Result<Ctrl> {
        Ctrl::connect(&self.home.join(SOCKET_NAME))
    }

    pub fn app(&self, request: Value) -> Result<Value> {
        let path = self.home.join("herdr-app.sock");
        let stream = UnixStream::connect(&path).map_err(|e| {
            Fail::new("no_instance", format!("no Herdr window answers at {}: {e}", path.display()))
                .hint("start Herdr, or spawn a hidden one with `herdr-ctrl spawn`")
        })?;
        stream.set_read_timeout(Some(Duration::from_secs(60)))?;
        let mut writer = stream.try_clone()?;
        writeln!(writer, "{request}")?;
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line)?;
        let reply: Value = serde_json::from_str(line.trim()).context("the Herdr window sent an unreadable reply")?;
        if let Some(error) = reply.get("error") {
            let message = error.as_str().map(str::to_owned).unwrap_or_else(|| error.to_string());
            return Err(fail("app", message));
        }
        Ok(reply)
    }
}

pub fn live_home() -> PathBuf {
    if let Some(home) = std::env::var_os("HERDR_APP_HOME") {
        return home.into();
    }
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".config"));
    config.join("herdr-app")
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/".into())
}

pub struct Ctrl {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    pub hello: Hello,
    window: Option<WindowInfo>,
}

impl Ctrl {
    pub fn connect(path: &Path) -> Result<Ctrl> {
        let stream = UnixStream::connect(path).map_err(|e| {
            Fail::new("no_ctrl", format!("no herdr-ctrl server at {}: {e}", path.display())).hint(
                "this Herdr was built without the ctrl feature or is not running; use the JosXa/herdr-app fork build, or `herdr-ctrl spawn --build <fork checkout>`",
            )
        })?;
        stream.set_read_timeout(Some(Duration::from_secs(60)))?;
        let writer = stream.try_clone()?;
        let mut ctrl = Ctrl {
            reader: BufReader::new(stream),
            writer,
            hello: Hello {
                protocol: 0,
                pid: 0,
                os: String::new(),
                version: String::new(),
            },
            window: None,
        };
        ctrl.hello = ctrl.call(&Request::Hello)?;
        if ctrl.hello.protocol != PROTOCOL_VERSION {
            return Err(Fail::new(
                "unsupported",
                format!("the window speaks protocol {}, this CLI speaks {PROTOCOL_VERSION}", ctrl.hello.protocol),
            )
            .hint("rebuild the CLI and the Herdr binary from the same herdr-ctrl revision")
            .into());
        }
        Ok(ctrl)
    }

    pub fn raw(&mut self, request: &Request) -> Result<Response> {
        writeln!(self.writer, "{}", serde_json::to_string(request)?)?;
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            return Err(fail("app_crash", "the Herdr window closed the connection"));
        }
        Ok(serde_json::from_str(line.trim())?)
    }

    pub fn call<T: DeserializeOwned>(&mut self, request: &Request) -> Result<T> {
        match self.raw(request)? {
            Response::Ok { ok } => Ok(serde_json::from_value(ok)?),
            Response::Err { error } => {
                let kind = match error.kind {
                    ErrorKind::Warming => "warming",
                    ErrorKind::NoWindow => "no_window",
                    ErrorKind::NotFound => "not_found",
                    ErrorKind::BadRequest => "bad_request",
                    ErrorKind::Failed => "failed",
                };
                Err(fail(kind, error.message))
            }
        }
    }

    pub fn window(&mut self) -> Result<WindowInfo> {
        if let Some(window) = &self.window {
            return Ok(window.clone());
        }
        let windows: Vec<WindowInfo> = self.call(&Request::Windows)?;
        let window = windows
            .iter()
            .find(|w| w.active)
            .or(windows.first())
            .cloned()
            .ok_or_else(|| fail("no_window", "the app has no open window"))?;
        self.window = Some(window.clone());
        Ok(window)
    }

    pub fn tree(&mut self) -> Result<RawTree> {
        let window = self.window()?.window;
        let start = Instant::now();
        loop {
            match self.call::<RawTree>(&Request::Tree { window }) {
                Err(e) if is_kind(&e, "warming") && start.elapsed() < Duration::from_secs(5) => {
                    std::thread::sleep(Duration::from_millis(30))
                }
                other => return other,
            }
        }
    }
}

pub fn is_kind(error: &anyhow::Error, kind: &str) -> bool {
    error.downcast_ref::<Fail>().is_some_and(|f| f.kind == kind)
}
