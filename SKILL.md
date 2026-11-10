# herdr-ctrl

Drive Herdr the way Playwright drives a browser: read the element tree, act on elements by ref or selector, and let the app dispatch the input in-process. Nothing touches the real pointer, keyboard or focus, and the window may be hidden or covered.

It needs a Herdr built with the ctrl server: the JosXa/herdr-app fork, or any checkout with the `herdr-ctrl-app` crate and `patches/gpui-pre-0.3.8-driver.patch`. Without it, commands fail with `no_ctrl`.

## Pick the target

- **Hidden instance** for anything that mutates: a rebuilt binary, a mod under development, restarts, fixtures, or a test you would otherwise do in a new window. `herdr-ctrl spawn` gives it its own home, state and core, on a private Xvfb display on Linux or as an invisible, never-focused app on macOS, and copies the live config unless you pass `--fresh`.
- **Live window** (`--on live`, the default) for observing the user's Herdr: `snapshot`, `find`, `get`, `screenshot`, `status`, `ls`. Input there is refused unless the current space is your scratch space (`scratch open`) or you pass `--allow-live`. A scratch space is visible to the user, so close it when done.

Every hidden instance has a lease (`--ttl`, default 30m) that each command renews; when it runs out, its supervisor tears the window, core and display down. Stop instances you are done with.

```sh
herdr-ctrl spawn --build /path/to/herdr-app-checkout   # or --bin, or herdr-app on PATH
export HERDR_CTRL_ON=h1 HERDR_CTRL_AGENT=<your terminal or task>
herdr-ctrl snapshot -i
herdr-ctrl stop h1
```

`spawn` prints the instance name. `restart` relaunches its window on the rebuilt binary; it restarts the core too unless you pass `--keep-core`. On the live window (`--on live`, the default) `restart` runs Herdr's `app.restart`, waits for the old window to quit and a new one to answer, and always keeps the core and its terminals; it needs a fork build with `app.restart` (personal-v0.1.13.37 or later). `exec -- hc ls` runs a command with the instance's environment. `opened` lists URLs and files the app tried to open; the instance never opens them.

## The loop

1. `snapshot -i` prints interactive elements as `@e12 button "Settings" ctx=Mod [focused]`. Refs stay stable for the same element across snapshots. Add `--within SEL` to narrow, `--source` for the Rust or mod source line, or drop `-i` for text lines too.
2. Act: `click`, `hover`, `drag A B`, `drop SEL FILE…`, `scroll down 3 --in SEL`, `type TEXT`, `fill SEL TEXT`, `press ctrl-shift-p`. Herdr commands work too: `do space.new name=test`, `commands`.
3. Verify with a fresh read: `wait` (idle), `wait SEL`, `wait --text T`, `wait SEL --gone`, `expect SEL --state focused`, `get SEL`, `status`, `screenshot -o shot.png [--element SEL] [--annotate]`.

Terminal panes appear as `textbox … ctx=Terminal` whose name is the screen text, so `wait --text` sees shell output. `read t3` and `send t3 TEXT` go through `hc`.

## Selectors

A selector is a ref (`@e12`), or steps joined by `>>`, each step narrowing to descendants of the previous match. A step combines any of: a role word with an optional name (`button "Next"`), `#id`, `ctx=Input`, `src=terminal.rs`, `nth=2`, or plain text. Names match exactly first, then as a case-insensitive substring. `text=` takes the rest of the step literally.

```sh
herdr-ctrl click 'ov-palette >> button "Split right"'
herdr-ctrl fill 'textbox ctx=Input' split
herdr-ctrl click 'button "Split" >> nth=2'
```

Actions refuse instead of guessing: `ambiguous` lists the candidates, `covered` names the element on top, `not_visible` means it is clipped or scrolled away, `stale_ref` means take a new snapshot.

## More input

- `click --button right|middle|back|forward`, `--double`, `--with ctrl,shift`, `--at 0.1,0.5` (fractions of the element's box), or `click 300,200` for a point.
- `type --keys` sends each character as a keystroke instead of inserting text.
- `input steps.json` replays raw events (`mouse_move`, `mouse_down`, `mouse_up`, `key_down`, `key_up`, `modifiers`, `scroll`, `text`, `file_drop`, `{"type":"wait","ms":100}`) and releases held keys and buttons when a step fails.
- `pick /some/folder` answers the next native file panel; `pick` alone cancels it; `picks` shows what the app asked for.
- `clipboard set --text T | --image a.png`, `clipboard get [--image out.png]`.
- `record start` / `record stop -o out.mp4` (ffmpeg).

## Mods

`mod check DIR` runs `herdr-app check`. `mod load DIR [--as NAME]` installs a working copy into the target's mods, keeping a backup; Herdr hot-reloads it. `mod restore NAME` puts the backup back. On a hidden instance, `spawn --mod DIR` loads it from the start.

## Output and errors

`--json` prints results as JSON and errors as `{"error":{"kind","message","hint"}}`. Exit codes: 2 usage, 3 not found / stale / ambiguous / covered / not visible, 4 timeout, 5 failed `expect`, 6 no instance or no ctrl server, 1 anything else. `capabilities` lists what this build supports. `mcp` serves every command as one MCP tool.
