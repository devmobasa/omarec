import QtQuick
import Quickshell
import Quickshell.Io

// omarec recording state service. Exactly one `omarec --json watch` consumer
// for the whole shell; the bar widget and panel read this service and route
// every mutation through it. Protocol v1: state is replaced only from a
// complete snapshot when the daemon lifetime changes, and stale windows
// (after lag or a dropped watch) disable mutations until a fresh snapshot.
//
// Lifecycle commands go through the omarec dispatcher script so owner
// markers, indicator refreshes, and legacy parity stay in one place. The
// paths are absolute: the session PATH resolves the bare script name to the
// legacy recorder.
Item {
  id: root

  // Injected by the shell service loader.
  property var shell: null

  readonly property string home: Quickshell.env("HOME")
  // Prefer a user install of omarec. /usr/bin/omarchy-capture-screenrecording
  // is Omarchy's legacy script until the omarec package owns that path, so
  // we only use /usr/bin when /usr/bin/omarec exists too. Never a bare name:
  // session PATH can resolve the dispatcher to the legacy recorder.
  property string binDir: ""
  readonly property string dispatcher: binDir.length > 0 ? binDir + "/omarchy-capture-screenrecording" : ""
  readonly property string omarecBin: binDir.length > 0 ? binDir + "/omarec" : ""

  property var snapshot: ({ "phase": "idle", "paused": false, "session_id": null })
  property string lastError: ""
  property bool stale: false
  property string daemonLifetimeId: ""
  property int watermark: 0
  property int reconnectDelayMs: 250
  property string lastSavedPath: ""

  readonly property bool connected: watcher.running
  readonly property string phase: (snapshot && snapshot.phase) ? snapshot.phase : "idle"
  readonly property bool paused: snapshot && snapshot.paused === true
  readonly property bool sessionActive: [
    "preparing", "launching", "recording", "paused",
    "stopping", "finalizing", "recovering"
  ].indexOf(phase) >= 0

  signal changed(var snapshot)

  function displayedSessionId() {
    return (snapshot && snapshot.session_id) ? String(snapshot.session_id) : ""
  }

  function formatElapsed(ms) {
    var total = Math.max(0, Math.floor(ms / 1000))
    var hours = Math.floor(total / 3600)
    var minutes = Math.floor((total % 3600) / 60)
    var seconds = total % 60
    var mm = (hours > 0 && minutes < 10 ? "0" : "") + minutes
    var ss = (seconds < 10 ? "0" : "") + seconds
    return (hours > 0 ? hours + ":" + mm : mm) + ":" + ss
  }

  function consume(line) {
    var envelope
    try {
      envelope = JSON.parse(line)
    } catch (error) {
      return
    }
    if (envelope.protocol !== 1) {
      lastError = "Unsupported omarec protocol: " + envelope.protocol
      return
    }
    var lifetime = envelope.daemon_lifetime_id || ""
    if (lifetime && lifetime !== daemonLifetimeId) {
      daemonLifetimeId = lifetime
      stale = false
      watermark = 0
    }
    var event = envelope.event
    if (!event) return
    if (event.type === "lag") {
      stale = true
      return
    }
    if (event.type === "snapshot") {
      if (event.watermark) watermark = event.watermark
      applySnapshot(event.snapshot)
      stale = false
      return
    }
    if (envelope.sequence && envelope.sequence <= watermark) return
    if (event.type === "state_changed") applySnapshot(event.snapshot)
    else if (event.type === "file_saved" && event.output) lastSavedPath = String(event.output)
  }

  function applySnapshot(next) {
    snapshot = next || ({ "phase": "idle" })
    lastError = (next && next.last_error) ? String(next.last_error) : ""
    // A completed session's output is the promoted recording; seeding from the
    // snapshot keeps "last recording" working across shell restarts.
    if (next && next.phase === "completed" && next.output) lastSavedPath = String(next.output)
    changed(snapshot)
  }

  // ---- command surface ------------------------------------------------------

  function startRegion() { runOmarec(startProc, [dispatcher]) }
  function startFullscreen() { runOmarec(startProc, [dispatcher, "--fullscreen", "--with-desktop-audio"]) }
  function startWebcam() {
    runOmarec(startProc, [dispatcher, "--with-webcam", "--with-desktop-audio", "--with-microphone-audio"])
  }
  function stopRecording() { runOmarec(controlProc, [dispatcher, "--stop"]) }
  function pauseToggle() {
    var session = displayedSessionId()
    var args = [omarecBin, "pause", "--toggle"]
    if (session.length > 0) args.push("--session", session)
    runOmarec(controlProc, args)
  }
  function openLastSaved() {
    if (!lastSavedPath) return
    var folder = lastSavedPath.substring(0, lastSavedPath.lastIndexOf("/"))
    run(controlProc, ["xdg-open", folder.length > 0 ? folder : lastSavedPath])
  }
  function trimLastSaved() {
    if (!lastSavedPath) return
    run(controlProc, ["omacut", lastSavedPath])
  }

  function runOmarec(proc, command) {
    if (root.binDir.length === 0) return
    run(proc, command)
  }

  function run(proc, command) {
    if (proc.running) return
    proc.command = command
    proc.running = true
  }

  Process { id: startProc }
  Process { id: controlProc }

  Process {
    id: localOmarecProbe
    running: true
    command: ["test", "-x", root.home + "/.local/bin/omarec"]
    onExited: function (code) {
      if (code === 0)
        root.useBinDir(root.home + "/.local/bin")
      else
        packagedOmarecProbe.running = true
    }
  }

  Process {
    id: packagedOmarecProbe
    running: false
    command: ["test", "-x", "/usr/bin/omarec"]
    onExited: function (code) {
      if (code === 0)
        root.useBinDir("/usr/bin")
    }
  }

  function useBinDir(dir) {
    root.binDir = dir
    if (!watcher.running)
      watcher.running = true
  }

  Process {
    id: watcher
    running: false
    command: [root.omarecBin, "--json", "watch"]
    stdout: SplitParser {
      onRead: data => root.consume(data)
    }
    onRunningChanged: {
      if (!running && root.binDir.length > 0) {
        root.stale = true
        reconnectTimer.interval = Math.min(4000,
          root.reconnectDelayMs + Math.floor(Math.random() * 200))
        reconnectTimer.restart()
      }
    }
  }

  Timer {
    id: reconnectTimer
    interval: root.reconnectDelayMs
    repeat: false
    onTriggered: if (root.binDir.length > 0) watcher.running = true
  }

  // Scriptable surface: omarchy-shell community.omarec status|stop|pause
  IpcHandler {
    target: "community.omarec"

    function status(): string {
      return JSON.stringify({
        phase: root.phase,
        paused: root.paused,
        session: root.displayedSessionId(),
        stale: root.stale,
        lastSaved: root.lastSavedPath
      })
    }

    function stop(): string {
      root.stopRecording()
      return root.phase
    }

    function pause(): string {
      root.pauseToggle()
      return root.phase
    }
  }
}
