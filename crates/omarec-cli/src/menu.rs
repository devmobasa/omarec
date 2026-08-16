//! Small GTK overlay for the same start/pause/stop intents as the bar dropdown.
//!
//! The daemon stays UI-free. This command draws its own window and then runs
//! the dispatcher / `omarec` argv the keybinds already use.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use gtk4::gdk::Key;
use gtk4::gio::ApplicationFlags;
use gtk4::glib::Propagation;
use gtk4::prelude::*;
use gtk4::{
    Align, Application, ApplicationWindow, Box as GtkBox, Button, CssProvider, EventControllerKey,
    Label, Orientation, STYLE_PROVIDER_PRIORITY_APPLICATION, Separator,
};
#[cfg(feature = "layer-shell")]
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use omarec_core::{SessionPhase, SessionSnapshot};

use super::CliError;

const MENU_CSS: &str = r"
window.omarec-menu {
  padding: 12px;
  border-radius: 8px;
}
window.omarec-menu label.title {
  font-weight: bold;
  font-size: 16pt;
}
window.omarec-menu label.subtitle {
  opacity: 0.85;
}
window.omarec-menu label.detail {
  opacity: 0.7;
  font-size: 10pt;
}
window.omarec-menu button.omarec-action {
  padding: 8px 12px;
  margin: 0;
  min-width: 260px;
  border: 1px solid alpha(@theme_fg_color, 0.45);
  background: transparent;
}
window.omarec-menu.compact {
  padding: 8px 10px;
  border-radius: 16px;
}
window.omarec-menu.compact button.omarec-tile {
  min-width: 76px;
  min-height: 64px;
  padding: 8px 10px;
  margin: 0;
  border: 1px solid alpha(@theme_fg_color, 0.45);
  background: transparent;
  border-radius: 12px;
}
window.omarec-menu.compact label.omarec-tile-icon {
  font-size: 20pt;
}
window.omarec-menu.compact label.omarec-tile-caption {
  font-size: 9pt;
  opacity: 0.85;
}
window.omarec-menu.compact label.subtitle {
  font-size: 10pt;
}
";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MenuAction {
    StartRegion,
    StartFullscreen,
    StartWebcam,
    PauseToggle,
    Stop,
    TrimLast,
    OpenFolder,
}

impl MenuAction {
    pub(crate) fn label(self, paused: bool) -> &'static str {
        match self {
            Self::StartRegion => "Record region or window",
            Self::StartFullscreen => "Fullscreen with desktop audio",
            Self::StartWebcam => "Region with webcam and microphone",
            Self::PauseToggle if paused => "Resume",
            Self::PauseToggle => "Pause",
            Self::Stop => "Stop",
            Self::TrimLast => "Trim last recording in Omacut",
            Self::OpenFolder => "Open recordings folder",
        }
    }

    fn icon(self, paused: bool) -> &'static str {
        match self {
            Self::StartRegion => "󰩭",
            Self::StartFullscreen => "󰍹",
            Self::StartWebcam => "󰄀",
            Self::PauseToggle if paused => "󰐊",
            Self::PauseToggle => "󰏤",
            Self::Stop => "󰙦",
            Self::TrimLast => "󰆐",
            Self::OpenFolder => "",
        }
    }

    fn caption(self, paused: bool) -> &'static str {
        match self {
            Self::StartRegion => "Region",
            Self::StartFullscreen => "Fullscreen",
            Self::StartWebcam => "Webcam",
            Self::PauseToggle if paused => "Resume",
            Self::PauseToggle => "Pause",
            Self::Stop => "Stop",
            Self::TrimLast => "Trim",
            Self::OpenFolder => "Folder",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MenuKind {
    Card,
    Compact,
}

#[derive(Clone, Debug)]
struct MenuModel {
    kind: MenuKind,
    subtitle: String,
    details: Vec<String>,
    items: Vec<MenuAction>,
    paused: bool,
}

impl MenuModel {
    fn from_snapshot(snapshot: &SessionSnapshot, compact: bool) -> Self {
        let saved = snapshot
            .output
            .as_ref()
            .is_some_and(|path| path.as_os_str() != "-");
        let active = snapshot.phase.is_active();
        Self {
            kind: if compact {
                MenuKind::Compact
            } else {
                MenuKind::Card
            },
            subtitle: subtitle(snapshot),
            details: details(snapshot),
            items: if compact {
                compact_actions(active)
            } else {
                actions(active, saved)
            },
            paused: snapshot.paused,
        }
    }
}

pub(crate) fn actions(active: bool, saved: bool) -> Vec<MenuAction> {
    let mut items = Vec::new();
    if active {
        items.push(MenuAction::PauseToggle);
        items.push(MenuAction::Stop);
    } else {
        items.extend([
            MenuAction::StartRegion,
            MenuAction::StartFullscreen,
            MenuAction::StartWebcam,
        ]);
    }
    if saved {
        items.push(MenuAction::TrimLast);
        items.push(MenuAction::OpenFolder);
    }
    items
}

pub(crate) fn compact_actions(active: bool) -> Vec<MenuAction> {
    actions(active, false)
}

pub(crate) async fn run_menu(
    socket: &Path,
    autostart: bool,
    print_only: bool,
    compact: bool,
) -> Result<(), CliError> {
    let snapshot = super::current_snapshot(socket, autostart)
        .await
        .unwrap_or_default();
    let model = MenuModel::from_snapshot(&snapshot, compact);
    if print_only {
        for action in &model.items {
            println!("{}", action.label(model.paused));
        }
        return Ok(());
    }
    let Some(choice) = tokio::task::block_in_place(|| show_window(&model)) else {
        return Ok(());
    };
    execute(choice, &snapshot)
}

fn subtitle(snapshot: &SessionSnapshot) -> String {
    let label = phase_label(snapshot.phase);
    if matches!(
        snapshot.phase,
        SessionPhase::Recording | SessionPhase::Paused
    ) && let Some(started) = snapshot.started_realtime_ms
        && let Some(elapsed) = elapsed_since(started)
    {
        format!("{label} — {elapsed}")
    } else {
        label.to_owned()
    }
}

fn phase_label(phase: SessionPhase) -> &'static str {
    match phase {
        SessionPhase::Recording => "Recording",
        SessionPhase::Paused => "Paused",
        SessionPhase::Idle => "Ready",
        SessionPhase::Preparing => "preparing",
        SessionPhase::Launching => "launching",
        SessionPhase::Stopping => "stopping",
        SessionPhase::Finalizing => "finalizing",
        SessionPhase::Recovering => "recovering",
        SessionPhase::Completed => "completed",
        SessionPhase::Cancelled => "cancelled",
        SessionPhase::Failed => "failed",
    }
}

fn details(snapshot: &SessionSnapshot) -> Vec<String> {
    let mut lines = Vec::new();
    if snapshot.phase.is_active() {
        if let Some(target) = snapshot
            .target_summary
            .as_deref()
            .filter(|text| !text.is_empty())
        {
            lines.push(format!("Target: {target}"));
        }
        let mut audio = format!(
            "Desktop audio {} · Microphone {}",
            if snapshot.desktop_audio { "on" } else { "off" },
            if snapshot.microphone { "on" } else { "off" },
        );
        if snapshot
            .webcam_summary
            .as_deref()
            .is_some_and(|text| !text.is_empty())
        {
            audio.push_str(" · Webcam");
        }
        lines.push(audio);
    }
    if let Some(error) = snapshot
        .last_error
        .as_deref()
        .filter(|text| !text.is_empty())
    {
        lines.push(error.to_owned());
    }
    lines
}

fn elapsed_since(started_ms: u64) -> Option<String> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    let now_ms = u64::try_from(now.as_millis()).unwrap_or(u64::MAX);
    Some(format_elapsed(now_ms.saturating_sub(started_ms)))
}

fn format_elapsed(ms: u64) -> String {
    let total = ms / 1000;
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let seconds = total % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

fn show_window(model: &MenuModel) -> Option<MenuAction> {
    let choice = Rc::new(Cell::new(None));
    let app = Application::builder()
        .application_id("org.omarec.Menu")
        .flags(ApplicationFlags::NON_UNIQUE)
        .build();
    let model = model.clone();
    let selected = Rc::clone(&choice);
    app.connect_activate(move |app| build_window(app, &model, &selected));
    // Application::run() forwards process argv, so `omarec menu` is treated as
    // a file to open and GIO prints "This application can not open files."
    app.run_with_args(&[] as &[&str]);
    choice.get()
}

fn build_window(app: &Application, model: &MenuModel, choice: &Rc<Cell<Option<MenuAction>>>) {
    let compact = model.kind == MenuKind::Compact;
    let window = ApplicationWindow::builder()
        .application(app)
        .title("Screen recording")
        .decorated(false)
        .resizable(false)
        .default_width(if compact { -1 } else { 320 })
        .build();
    window.add_css_class("omarec-menu");
    if compact {
        window.add_css_class("compact");
    }
    #[cfg(feature = "layer-shell")]
    {
        window.init_layer_shell();
        window.set_namespace("omarec-menu");
        window.set_layer(Layer::Overlay);
        window.set_keyboard_mode(KeyboardMode::Exclusive);
        window.set_anchor(Edge::Top, true);
        window.set_margin(Edge::Top, 48);
    }

    let provider = CssProvider::new();
    provider.load_from_data(MENU_CSS);
    if let Some(display) = gtk4::gdk::Display::default() {
        gtk4::style_context_add_provider_for_display(
            &display,
            &provider,
            STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

    let content = if compact {
        compact_content(app, &window, model, choice)
    } else {
        card_content(app, &window, model, choice)
    };
    bind_keys(app, &window, choice, model.items.clone());
    window.set_child(Some(&content));
    window.present();
}

fn card_content(
    app: &Application,
    window: &ApplicationWindow,
    model: &MenuModel,
    choice: &Rc<Cell<Option<MenuAction>>>,
) -> GtkBox {
    let column = GtkBox::new(Orientation::Vertical, 8);
    column.set_margin_start(4);
    column.set_margin_end(4);

    let title = Label::new(Some("Screen recording"));
    title.add_css_class("title");
    title.set_halign(Align::Start);
    column.append(&title);

    let subtitle = Label::new(Some(&model.subtitle));
    subtitle.add_css_class("subtitle");
    subtitle.set_halign(Align::Start);
    column.append(&subtitle);

    for line in &model.details {
        let detail = Label::new(Some(line));
        detail.add_css_class("detail");
        detail.set_halign(Align::Start);
        detail.set_wrap(true);
        detail.set_xalign(0.0);
        column.append(&detail);
    }

    column.append(&Separator::new(Orientation::Horizontal));

    for &action in &model.items {
        column.append(&action_button(app, window, choice, action, model.paused));
    }
    column
}

fn compact_content(
    app: &Application,
    window: &ApplicationWindow,
    model: &MenuModel,
    choice: &Rc<Cell<Option<MenuAction>>>,
) -> GtkBox {
    let column = GtkBox::new(Orientation::Vertical, 6);
    if model.subtitle != "Ready" {
        let subtitle = Label::new(Some(&model.subtitle));
        subtitle.add_css_class("subtitle");
        subtitle.set_halign(Align::Center);
        column.append(&subtitle);
    }
    let row = GtkBox::new(Orientation::Horizontal, 6);
    row.set_halign(Align::Center);
    for &action in &model.items {
        row.append(&tile_button(app, window, choice, action, model.paused));
    }
    column.append(&row);
    column
}

fn action_button(
    app: &Application,
    window: &ApplicationWindow,
    choice: &Rc<Cell<Option<MenuAction>>>,
    action: MenuAction,
    paused: bool,
) -> Button {
    let button = Button::new();
    button.add_css_class("omarec-action");
    button.set_hexpand(true);

    let row = GtkBox::new(Orientation::Horizontal, 8);
    let icon = action.icon(paused);
    if !icon.is_empty() {
        let glyph = Label::new(Some(icon));
        glyph.set_halign(Align::Start);
        row.append(&glyph);
    }
    let text = Label::new(Some(action.label(paused)));
    text.set_halign(Align::Start);
    text.set_hexpand(true);
    text.set_xalign(0.0);
    row.append(&text);
    button.set_child(Some(&row));
    connect_choice(app, window, choice, &button, action);
    button
}

fn tile_button(
    app: &Application,
    window: &ApplicationWindow,
    choice: &Rc<Cell<Option<MenuAction>>>,
    action: MenuAction,
    paused: bool,
) -> Button {
    let button = Button::new();
    button.add_css_class("omarec-tile");
    button.set_tooltip_text(Some(action.label(paused)));

    let column = GtkBox::new(Orientation::Vertical, 2);
    column.set_halign(Align::Center);
    let icon = Label::new(Some(action.icon(paused)));
    icon.add_css_class("omarec-tile-icon");
    icon.set_halign(Align::Center);
    let caption = Label::new(Some(action.caption(paused)));
    caption.add_css_class("omarec-tile-caption");
    caption.set_halign(Align::Center);
    column.append(&icon);
    column.append(&caption);
    button.set_child(Some(&column));
    connect_choice(app, window, choice, &button, action);
    button
}

fn connect_choice(
    app: &Application,
    window: &ApplicationWindow,
    choice: &Rc<Cell<Option<MenuAction>>>,
    button: &Button,
    action: MenuAction,
) {
    let choice = Rc::clone(choice);
    let app = app.clone();
    let window = window.clone();
    button.connect_clicked(move |_| {
        choice.set(Some(action));
        window.close();
        app.quit();
    });
}

fn bind_keys(
    app: &Application,
    window: &ApplicationWindow,
    choice: &Rc<Cell<Option<MenuAction>>>,
    items: Vec<MenuAction>,
) {
    let keys = EventControllerKey::new();
    let app = app.clone();
    let window_for_keys = window.clone();
    let choice = Rc::clone(choice);
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == Key::Escape {
            app.quit();
            return Propagation::Stop;
        }
        if let Some(index) = digit_index(key)
            && let Some(action) = items.get(index).copied()
        {
            choice.set(Some(action));
            window_for_keys.close();
            app.quit();
            return Propagation::Stop;
        }
        Propagation::Proceed
    });
    window.add_controller(keys);
}

fn digit_index(key: Key) -> Option<usize> {
    Some(match key {
        Key::_1 | Key::KP_1 => 0,
        Key::_2 | Key::KP_2 => 1,
        Key::_3 | Key::KP_3 => 2,
        Key::_4 | Key::KP_4 => 3,
        Key::_5 | Key::KP_5 => 4,
        _ => return None,
    })
}

fn execute(action: MenuAction, snapshot: &SessionSnapshot) -> Result<(), CliError> {
    match action {
        MenuAction::StartRegion => run(&dispatcher()?, &[] as &[&str])?,
        MenuAction::StartFullscreen => {
            run(&dispatcher()?, &["--fullscreen", "--with-desktop-audio"])?;
        }
        MenuAction::StartWebcam => run(
            &dispatcher()?,
            &[
                "--with-webcam",
                "--with-desktop-audio",
                "--with-microphone-audio",
            ],
        )?,
        MenuAction::PauseToggle => {
            let session = snapshot.session_id.map(|id| id.to_string());
            let mut args = vec!["pause", "--toggle"];
            if let Some(session) = session.as_deref() {
                args.push("--session");
                args.push(session);
            }
            run(&omarec_bin(), &args)?;
        }
        MenuAction::Stop => run(&dispatcher()?, &["--stop"])?,
        MenuAction::TrimLast => {
            let path = snapshot.output.as_ref().ok_or_else(|| {
                CliError::InvalidArguments("there is no saved recording to trim".to_owned())
            })?;
            run(Path::new("omacut"), &[path.as_path()])?;
        }
        MenuAction::OpenFolder => {
            let folder = snapshot
                .output
                .as_ref()
                .and_then(|path| path.parent())
                .map_or_else(recordings_dir, Path::to_path_buf);
            run(Path::new("xdg-open"), &[folder.as_path()])?;
        }
    }
    Ok(())
}

fn run<A: AsRef<std::ffi::OsStr>>(program: &Path, args: &[A]) -> Result<(), CliError> {
    let status = Command::new(program)
        .args(args)
        .status()
        .map_err(|error| CliError::Command(program.to_path_buf(), error))?;
    if status.success() {
        Ok(())
    } else {
        Err(CliError::CommandFailed(
            program.to_path_buf(),
            status.code(),
        ))
    }
}

fn dispatcher() -> Result<PathBuf, CliError> {
    if let Ok(path) = std::env::var("OMAREC_DISPATCHER") {
        return Ok(PathBuf::from(path));
    }
    let candidates = [
        std::env::current_exe()
            .ok()
            .as_deref()
            .and_then(Path::parent)
            .and_then(omarec_dispatcher_in),
        home_bin_dir().as_deref().and_then(omarec_dispatcher_in),
        omarec_dispatcher_in(Path::new("/usr/bin")),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|path| path.is_file())
        .ok_or_else(|| {
            CliError::InvalidArguments(
                "omarchy-capture-screenrecording not found; set OMAREC_DISPATCHER".to_owned(),
            )
        })
}

/// Omarchy ships `/usr/bin/omarchy-capture-screenrecording` as the legacy
/// recorder. An omarec install is the dispatcher plus the `-omarec` sibling.
fn omarec_dispatcher_in(dir: &Path) -> Option<PathBuf> {
    let dispatcher = dir.join("omarchy-capture-screenrecording");
    let native = dir.join("omarchy-capture-screenrecording-omarec");
    (dispatcher.is_file() && native.is_file()).then_some(dispatcher)
}

fn omarec_bin() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("omarec"))
}

fn home_bin_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/bin"))
}

fn recordings_dir() -> PathBuf {
    std::env::var_os("HOME").map_or_else(
        || PathBuf::from("."),
        |home| PathBuf::from(home).join("Videos/Screenrecordings"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_menu_lists_start_intents() {
        let labels: Vec<_> = actions(false, false)
            .into_iter()
            .map(|action| action.label(false))
            .collect();
        assert_eq!(
            labels,
            [
                "Record region or window",
                "Fullscreen with desktop audio",
                "Region with webcam and microphone",
            ]
        );
    }

    #[test]
    fn recording_menu_lists_pause_and_stop() {
        let labels: Vec<_> = actions(true, true)
            .into_iter()
            .map(|action| action.label(false))
            .collect();
        assert_eq!(
            labels,
            [
                "Pause",
                "Stop",
                "Trim last recording in Omacut",
                "Open recordings folder",
            ]
        );
        assert_eq!(MenuAction::PauseToggle.label(true), "Resume");
        assert_eq!(MenuAction::PauseToggle.icon(true), "󰐊");
    }

    #[test]
    fn compact_menu_omits_trim_and_folder() {
        let labels: Vec<_> = compact_actions(false)
            .into_iter()
            .map(|action| action.caption(false))
            .collect();
        assert_eq!(labels, ["Region", "Fullscreen", "Webcam"]);
        let recording: Vec<_> = compact_actions(true)
            .into_iter()
            .map(|action| action.caption(false))
            .collect();
        assert_eq!(recording, ["Pause", "Stop"]);
    }

    #[test]
    fn packaged_dispatcher_requires_omarec_sibling() {
        assert_eq!(
            PathBuf::from("/usr/bin/omarchy-capture-screenrecording-omarec").is_file(),
            omarec_dispatcher_in(Path::new("/usr/bin")).is_some()
        );
    }

    #[test]
    fn completed_phase_matches_the_bar_subtitle() {
        assert_eq!(phase_label(SessionPhase::Completed), "completed");
        assert_eq!(phase_label(SessionPhase::Idle), "Ready");
        assert_eq!(format_elapsed(65_000), "1:05");
        assert_eq!(format_elapsed(3_661_000), "1:01:01");
    }
}
