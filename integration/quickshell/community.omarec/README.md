# Screen Recording (omarec)

Live recording state for [omarec](https://github.com/devmobasa/omarec) in the
Omarchy shell: a bar chip that shows a dim glyph when idle and the elapsed
`mm:ss` while recording, with an anchored dropdown for start, pause/resume,
and stop.

## Requires

- `omarec` and `omarecd` installed. The plugin prefers `~/.local/bin/omarec`
  (README / cargo install) and uses `/usr/bin` only when `/usr/bin/omarec`
  exists. It never calls a bare dispatcher name: session PATH can resolve
  that to Omarchy's legacy recorder.
- The omarec user service (`omarec.service`); the watch stream autostarts it.

## Bar chip

| Input | Action |
|---|---|
| Left click | Open the dropdown |
| Right click | Pause / resume the active recording |
| Middle click | Stop the active recording |

## Dropdown

Anchored under the chip, dismissed by Escape or an outside click.

- Idle: start buttons for region/window, fullscreen with desktop audio, and
  region with webcam + microphone. Start closes the dropdown first so the
  region picker has the screen.
- Active: pause/resume and stop, live elapsed time, target/audio info.
- After a save: "Open recordings folder".

## Scriptable surface

```bash
omarchy-shell community.omarec status
omarchy-shell community.omarec pause
omarchy-shell community.omarec stop
```

## Install

From the omarec checkout, `./install-omarchy` (or `bash scripts/install-user.sh`)
copies this plugin after installing binaries. To install only the plugin, copy
this folder to
`~/.config/omarchy/plugins/community.omarec/`, then:

```bash
omarchy plugin validate ~/.config/omarchy/plugins/community.omarec
omarchy plugin enable community.omarec
omarchy-shell shell rescanPlugins
omarchy restart shell
```

Bar widgets do not hot-reload: a running chip keeps old QML until the shell restarts.
`omarchy restart shell` refuses while the session is locked.

The packaged copy lives at `/usr/share/omarec/quickshell/community.omarec/`.
Binaries: `~/.local/bin` first if `omarec` is there, else `/usr/bin` if
`/usr/bin/omarec` exists. Bare dispatcher names are unsafe on Omarchy.
