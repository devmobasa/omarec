import QtQuick
import qs.Commons
import qs.Ui

// omarec bar chip: dim glyph when idle, urgent glyph plus elapsed mm:ss while
// recording, dimmed while paused. Left click opens an anchored dropdown with
// the start intents (or live controls while recording), right click
// pauses/resumes, middle click stops.
BarWidget {
  id: root
  moduleName: "community.omarec"

  property var recorder: null
  property int serviceTries: 0
  property double nowMs: Date.now()
  property bool opened: false
  property double pauseAccumulatedMs: 0
  property double pauseBeganMs: 0
  property string trackedSession: ""

  // open/close are the bar host's summon interface; toggle drives the chip.
  function open() { opened = true }
  function close() { opened = false }
  function toggle() { opened = !opened }

  // Bounded service resolution; plugin load order at shell startup is not
  // guaranteed.
  Timer {
    interval: 250
    repeat: true
    running: root.recorder === null && root.serviceTries < 40
    onTriggered: {
      root.serviceTries++
      var host = root.bar && root.bar.shell
        && typeof root.bar.shell.serviceFor === "function" ? root.bar.shell : null
      var service = host ? host.serviceFor("community.omarec") : null
      if (service) root.recorder = service
    }
  }

  readonly property string phase: recorder ? recorder.phase : "idle"
  readonly property bool sessionActive: recorder !== null && recorder.sessionActive
  readonly property bool timing: phase === "recording" || phase === "paused"
  readonly property var snap: recorder && recorder.snapshot ? recorder.snapshot : ({})

  // Tick only while GSR is actually capturing. Wall-clock from start would keep
  // running through pause and then jump on resume.
  Timer {
    running: root.phase === "recording"
    interval: 1000
    repeat: true
    triggeredOnStart: true
    onTriggered: root.nowMs = Date.now()
  }

  function syncPauseClock() {
    var session = root.recorder && root.recorder.displayedSessionId
      ? String(root.recorder.displayedSessionId()) : ""
    if (session !== root.trackedSession) {
      root.trackedSession = session
      root.pauseAccumulatedMs = 0
      root.pauseBeganMs = 0
    }
    if (root.phase === "paused") {
      if (root.pauseBeganMs === 0)
        root.pauseBeganMs = Date.now()
      return
    }
    if (root.pauseBeganMs !== 0) {
      root.pauseAccumulatedMs += Math.max(0, Date.now() - root.pauseBeganMs)
      root.pauseBeganMs = 0
    }
  }

  onPhaseChanged: root.syncPauseClock()
  onRecorderChanged: root.syncPauseClock()

  function glyphFor(phase) {
    switch (phase) {
    case "paused": return "󰏤"
    case "recording": return "󰑊"
    case "preparing":
    case "launching":
    case "stopping":
    case "finalizing": return "󰔟"
    case "recovering": return "󰁪"
    case "failed": return "󰅙"
    default: return "󰻃"
    }
  }

  readonly property string elapsedText: {
    if (!timing || !recorder) return ""
    var startedMs = snap.started_realtime_ms || 0
    if (!startedMs) return ""
    var endMs = (root.phase === "paused" && root.pauseBeganMs)
      ? root.pauseBeganMs : root.nowMs
    return recorder.formatElapsed(Math.max(0, endMs - startedMs - root.pauseAccumulatedMs))
  }

  // Start intents close the dropdown first: the region picker needs the
  // screen, and the always-loaded service owns the process, so closing here
  // never kills the command.
  function startAndClose(startFunction) {
    root.close()
    if (root.recorder) startFunction()
  }

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  WidgetButton {
    id: button
    bar: root.bar
    active: root.phase === "recording"
    dimmed: root.phase === "idle" || root.phase === "paused"
    text: root.elapsedText.length > 0
      ? root.glyphFor(root.phase) + " " + root.elapsedText
      : root.glyphFor(root.phase)
    tooltipText: {
      if (root.phase === "recording")
        return "Recording " + root.elapsedText
          + " — click for controls · right-click pauses · middle-click stops"
      if (root.phase === "paused")
        return "Recording paused — click for controls · right-click resumes"
      if (root.phase === "idle")
        return "Screen recording — click to start"
      return "Screen recording: " + root.phase
    }
    onPressed: function (mouseButton) {
      if (mouseButton === Qt.MiddleButton && root.sessionActive) root.recorder.stopRecording()
      else if (mouseButton === Qt.RightButton && root.sessionActive) root.recorder.pauseToggle()
      else root.toggle()
    }
  }

  KeyboardPanel {
    id: dropdown
    anchorItem: button
    owner: root
    bar: root.bar
    open: root.opened
    focusTarget: keyCatcher
    contentWidth: dropdown.fittedContentWidth(Style.space(300))
    contentHeight: dropdown.fittedContentHeight(dropdownColumn.implicitHeight, Style.space(480))

    PanelKeyCatcher {
      id: keyCatcher
      anchors.fill: parent
      onCloseRequested: root.close()

      Column {
        id: dropdownColumn
        width: parent.width
        spacing: Style.space(8)

        Text {
          text: "Screen recording"
          color: Color.popups.text
          font.family: Style.font.family
          font.pixelSize: Style.font.heading
          font.bold: true
        }

        Text {
          width: parent.width
          text: {
            var label = root.phase
            if (root.phase === "recording") label = "Recording"
            else if (root.phase === "paused") label = "Paused"
            else if (root.phase === "idle") label = "Ready"
            return label + (root.elapsedText ? " — " + root.elapsedText : "")
          }
          color: root.phase === "recording" ? Color.accent : Qt.darker(Color.popups.text, 1.2)
          font.family: Style.font.family
          font.pixelSize: Style.font.body
        }

        Text {
          visible: root.sessionActive && (root.snap.target_summary || "").length > 0
          width: parent.width
          text: "Target: " + (root.snap.target_summary || "")
          color: Qt.darker(Color.popups.text, 1.35)
          font.family: Style.font.family
          font.pixelSize: Style.font.bodySmall
          wrapMode: Text.WordWrap
        }

        Text {
          visible: root.sessionActive
          width: parent.width
          text: "Desktop audio " + (root.snap.desktop_audio ? "on" : "off")
            + " · Microphone " + (root.snap.microphone ? "on" : "off")
            + ((root.snap.webcam_summary || "").length > 0 ? " · Webcam" : "")
          color: Qt.darker(Color.popups.text, 1.35)
          font.family: Style.font.family
          font.pixelSize: Style.font.bodySmall
          wrapMode: Text.WordWrap
        }

        Text {
          visible: root.recorder !== null && root.recorder.stale === true
          width: parent.width
          text: "Snapshot is stale; waiting for the daemon"
          color: Color.urgent
          font.family: Style.font.family
          font.pixelSize: Style.font.bodySmall
          wrapMode: Text.WordWrap
        }

        Text {
          visible: root.recorder !== null && root.recorder.lastError.length > 0
          width: parent.width
          text: root.recorder ? root.recorder.lastError : ""
          color: Color.urgent
          font.family: Style.font.family
          font.pixelSize: Style.font.bodySmall
          wrapMode: Text.WordWrap
        }

        PanelSeparator { width: parent.width }

        // Start intents: only when nothing is active.
        Column {
          visible: !root.sessionActive
          width: parent.width
          spacing: Style.space(4)

          Button {
            width: parent.width
            bordered: true
            iconText: "󰩭"
            text: "Record region or window"
            onClicked: root.startAndClose(function () { root.recorder.startRegion() })
          }
          Button {
            width: parent.width
            bordered: true
            iconText: "󰍹"
            text: "Fullscreen with desktop audio"
            onClicked: root.startAndClose(function () { root.recorder.startFullscreen() })
          }
          Button {
            width: parent.width
            bordered: true
            iconText: "󰄀"
            text: "Region with webcam and microphone"
            onClicked: root.startAndClose(function () { root.recorder.startWebcam() })
          }
        }

        // Live controls: only while a session is active.
        Row {
          visible: root.sessionActive
          spacing: Style.space(6)

          Button {
            bordered: true
            iconText: root.phase === "paused" ? "󰐊" : "󰏤"
            text: root.phase === "paused" ? "Resume" : "Pause"
            enabled: root.recorder !== null && !root.recorder.stale
              && (root.phase === "recording" || root.phase === "paused")
            onClicked: if (root.recorder) root.recorder.pauseToggle()
          }
          Button {
            bordered: true
            iconText: "󰙦"
            text: "Stop"
            enabled: root.recorder !== null && !root.recorder.stale
            onClicked: {
              if (root.recorder) root.recorder.stopRecording()
              root.close()
            }
          }
        }

        Column {
          visible: root.recorder !== null && root.recorder.lastSavedPath.length > 0
          width: parent.width
          spacing: Style.space(4)

          Button {
            width: parent.width
            bordered: true
            iconText: "󰆐"
            text: "Trim last recording in Omacut"
            onClicked: {
              if (root.recorder) root.recorder.trimLastSaved()
              root.close()
            }
          }
          Button {
            width: parent.width
            bordered: true
            iconText: ""
            text: "Open recordings folder"
            onClicked: {
              if (root.recorder) root.recorder.openLastSaved()
              root.close()
            }
          }
        }
      }
    }
  }
}
