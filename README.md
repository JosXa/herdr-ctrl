# herdr-ctrl

Agent control for [Herdr](https://github.com/herdrdev/herdr-app), built like browser automation: read the window's element tree, act on elements by ref or selector, and let the app dispatch the input in-process. It drives your running Herdr without taking the pointer or focus, and it runs hidden Herdr instances (on their own Xvfb displays on Linux, concealed on macOS) so tests stop opening windows on your desktop.

```console
$ herdr-ctrl spawn --build ~/src/herdr-app
spawned h1 on :90 (1400x900); drive it with --on h1 or HERDR_CTRL_ON=h1
$ export HERDR_CTRL_ON=h1
$ herdr-ctrl press ctrl-shift-p
$ herdr-ctrl fill 'textbox ctx=Input' split
$ herdr-ctrl click 'button "Split right"'
$ herdr-ctrl snapshot -i
window 4294967297 "" 1400x900
  @e1 scroll "herdr" ctx=Herdr
    …
          @e17 button "space-new"
$ herdr-ctrl screenshot -o shot.png --annotate
$ herdr-ctrl stop h1
```

[SKILL.md](SKILL.md) is the agent guide (`herdr-ctrl skill` prints it). `herdr-ctrl mcp` serves the same commands as one MCP tool.

## Parts

| Crate | Role |
| --- | --- |
| [`crates/cli`](crates/cli) | `herdr-ctrl`: the client, selectors, snapshots, and the hidden-instance supervisor. No GPUI dependency. |
| [`crates/app`](crates/app) | `herdr-ctrl-app`: the server inside Herdr. Serves `$HERDR_APP_HOME/herdr-ctrl.sock`, one JSON line per request. |
| [`crates/protocol`](crates/protocol) | The wire types both sides share. |
| [`patches/gpui-pre-0.3.8-driver.patch`](patches/gpui-pre-0.3.8-driver.patch) | Records every painted element (bounds, id, source, key context, handlers, text) and adds text-input and file-panel hooks to GPUI. |

## Adding it to a Herdr build

1. Apply the driver patch to the vendored GPUI: `git apply --directory=vendor/gpui-pre patches/gpui-pre-0.3.8-driver.patch`.
2. Depend on the server crate behind a feature:

   ```toml
   [target.'cfg(unix)'.dependencies]
   herdr-ctrl-app = { git = "https://github.com/JosXa/herdr-ctrl", optional = true }

   [features]
   default = ["ctrl"]
   ctrl = ["dep:herdr-ctrl-app"]
   ```

3. Start it after the mods in `main.rs`:

   ```rust
   #[cfg(all(unix, feature = "ctrl"))]
   herdr_ctrl_app::init(cx, mods::app_dir().join("herdr-ctrl.sock"), env!("CARGO_PKG_VERSION"));
   ```

The [JosXa/herdr-app](https://github.com/JosXa/herdr-app) fork carries these changes. The `crates/app` crate is outside the workspace because it only builds against the patched GPUI.

Install the CLI with `cargo install --git https://github.com/JosXa/herdr-ctrl herdr-ctrl`. On Linux, hidden instances need `Xvfb` on `PATH` (or `HERDR_CTRL_XVFB`). On macOS they need nothing extra: the app runs with the Prohibited activation policy, so it never shows in the Dock or takes focus, and its windows are transparent and click-through while it keeps drawing them. `record` needs `ffmpeg`.

## Credits

The driver patch, the in-app server and the snapshot logic started from [wsp-labs/gpui-agent](https://github.com/wsp-labs/gpui-agent) (Apache-2.0). See [NOTICE](NOTICE).
