//! FerroUI desktop shell: connect/trust, rooms, chat, history, voice, video, files and control.

#![cfg(windows)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod icons;
mod ui_kit;

use ferroui_base::input::{
    InputElement, Key, KeyEventArgs, PointerEventArgs, PointerPressedEventArgs,
    PointerReleasedEventArgs, PointerWheelEventArgs,
};
use ferroui_base::interactivity::RoutedEventHandlerToken;
use ferroui_base::layout::{HorizontalAlignment, Orientation, VerticalAlignment};
use ferroui_base::media::Brushes;
use ferroui_base::threading::{DispatcherPriority, DispatcherTimer};
use ferroui_base::{
    CornerRadius, FerroObjectImpl, Ref, Thickness, ferro_class, ferro_impl_classes, instantiate,
};
use ferroui_controls::primitives::ScrollBarVisibility;
use ferroui_controls::{
    AppBuilder, Application, ApplicationImpl, ApplicationImplExt, Border, Button, CheckBox,
    ColumnDefinition, ColumnDefinitions, Control, Grid, GridLength, GridUnitType, NewApplication,
    RowDefinition, RowDefinitions, ScrollViewer, StackPanel, TextBlock, TextBox, Window,
};
use ferroui_desktop::AppBuilderDesktopExtensions;
use ferroui_media::MediaSubscription;
use ferroui_media_controls::TexturePresenter;
use ferroui_themes_fluent::FluentTheme;
use rust_chat_client_core::{
    ChatSnapshot, ClientHandle, ClientSettings, ConnectRequest, ConnectionMemory, ConnectionState,
    update,
};
use rust_chat_desktop::control::{DEFAULT_LEASE_SECS, InputEvent, RemoteButton, countdown};
use rust_chat_desktop::files::{FilePrefs, human_size};
use rust_chat_desktop::video_surface::{VideoSurface, build_presenter};
use rust_chat_desktop::voice::{self, ShareSettings, VoiceCommand, VoiceController, VoiceEvent};
use rust_chat_media_desktop::audio::AudioIo;
use rust_chat_media_desktop::capture::{CaptureSource, SourceInfo, enumerate_sources};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
type UpdateResult = Result<Option<update::Release>, String>;

struct Startup {
    settings: ClientSettings,
    password: Option<String>,
    client: ClientHandle,
    memory: ConnectionMemory,
    runtime: tokio::runtime::Handle,
}

thread_local! {
    static STARTUP: RefCell<Option<Startup>> = const { RefCell::new(None) };
    static TIMER: RefCell<Option<Rc<DispatcherTimer>>> = const { RefCell::new(None) };
}

#[repr(C)]
pub struct App {
    base: Application,
}

ferro_class!(App: Application);
ferro_impl_classes!(App: FerroObjectImpl);

impl NewApplication for App {
    fn new_application() -> Ref<Self> {
        instantiate(Self {
            base: Application::construct(),
        })
    }
}

impl ApplicationImpl for App {
    fn initialize(this: &Self) {
        Self::parent_initialize(this);
        this.set_name(Some("rust-chat".to_string()));
        this.styles().add(FluentTheme::new().as_style());
    }

    fn on_framework_initialization_completed(this: &Self) {
        let lifetime = this.application_lifetime();
        if let Some(desktop) = lifetime
            .as_ref()
            .and_then(|lifetime| lifetime.as_classic_desktop_style_application_lifetime())
        {
            desktop.set_main_window(Some(create_main_window()));
        }
        Self::parent_on_framework_initialization_completed(this);
    }
}

const PRESENTER_WIDTH: f64 = 640.0;
const PRESENTER_HEIGHT: f64 = 360.0;

/// Width, height, fps and bitrate choices offered by the Share quality button.
const SHARE_PRESETS: &[(u32, u32, u32, u32)] = &[
    (1280, 720, 5, 1_000_000),
    (1280, 720, 10, 1_500_000),
    (1280, 720, 15, 2_000_000),
    (1280, 720, 30, 3_000_000),
    (1920, 1080, 5, 2_000_000),
    (1920, 1080, 10, 3_000_000),
    (1920, 1080, 15, 4_000_000),
    (2560, 1440, 5, 3_500_000),
    (2560, 1440, 10, 5_000_000),
];

fn preset_label(preset: (u32, u32, u32, u32)) -> String {
    format!("{}x{}@{}", preset.0, preset.1, preset.2)
}

/// A pickable audio device, including the "system default" entry.
struct DeviceChoice {
    id: Option<String>,
    label: String,
}

fn device_choices(devices: Vec<rust_chat_media::AudioDevice>) -> Vec<DeviceChoice> {
    let mut choices = vec![DeviceChoice {
        id: None,
        label: "System default".to_owned(),
    }];
    for device in devices {
        choices.push(DeviceChoice {
            id: Some(device.id),
            label: if device.is_default {
                format!("{} (default)", device.name)
            } else {
                device.name
            },
        });
    }
    choices
}

fn device_index(choices: &[DeviceChoice], id: Option<&str>) -> usize {
    id.and_then(|id| {
        choices
            .iter()
            .position(|choice| choice.id.as_deref() == Some(id))
    })
    .unwrap_or(0)
}

/// Local wall-clock time for a chat timestamp (milliseconds since the Unix epoch).
fn local_clock(ts_ms: i64, clock_24h: bool) -> String {
    use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
    // 100 ns ticks between 1601-01-01 and 1970-01-01.
    const EPOCH_OFFSET: i64 = 116_444_736_000_000_000;
    let ticks = ts_ms.saturating_mul(10_000).saturating_add(EPOCH_OFFSET);
    let utc_filetime = FILETIME {
        dwLowDateTime: (ticks & 0xffff_ffff) as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    let mut utc = SYSTEMTIME::default();
    let mut local = SYSTEMTIME::default();
    // SAFETY: both calls only read their input structs and write the output structs.
    unsafe {
        if FileTimeToSystemTime(&utc_filetime, &mut utc).is_err()
            || SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).is_err()
        {
            return String::new();
        }
    }
    if clock_24h {
        format_clock(local.wHour, local.wMinute, true)
    } else {
        format_clock(local.wHour, local.wMinute, false)
    }
}

fn format_clock(hour: u16, minute: u16, clock_24h: bool) -> String {
    if clock_24h {
        return format!("{hour:02}:{minute:02}");
    }
    let suffix = if hour < 12 { "AM" } else { "PM" };
    let hour = match hour % 12 {
        0 => 12,
        other => other,
    };
    format!("{hour}:{minute:02} {suffix}")
}

struct VoiceRuntime {
    audio: AudioIo,
    controller: VoiceController,
    mic_muted: Cell<bool>,
    speaker_muted: Cell<bool>,
    sharing: Cell<bool>,
    room: String,
    /// Levels keyed by session id; the empty key is the local microphone.
    levels: RefCell<HashMap<String, (f32, Instant)>>,
    /// Compact ICE labels such as `relay 34 ms`.
    stats: RefCell<HashMap<String, String>>,
    members_signature: RefCell<String>,
}

struct Ui {
    client: ClientHandle,
    memory: ConnectionMemory,
    runtime: tokio::runtime::Handle,
    settings: RefCell<ClientSettings>,
    connect_panel: Ref<StackPanel>,
    room_view: Ref<Grid>,
    stage_frame: Ref<Border>,
    file_card: Ref<Border>,
    share_card: Ref<Border>,
    server_line: Ref<TextBlock>,
    settings_nav: Ref<Button>,
    disconnect_nav: Ref<Button>,
    host: Ref<TextBox>,
    port: Ref<TextBox>,
    name: Ref<TextBox>,
    password: Ref<TextBox>,
    tls: Ref<CheckBox>,
    remember: Ref<CheckBox>,
    connect_status: Ref<TextBlock>,
    error_text: Ref<TextBlock>,
    trust_panel: Ref<StackPanel>,
    trust_text: Ref<TextBlock>,
    room_title: Ref<TextBlock>,
    room_status: Ref<TextBlock>,
    members_panel: Ref<StackPanel>,
    members_sig: RefCell<String>,
    rooms_panel: Ref<StackPanel>,
    feed_panel: Ref<StackPanel>,
    feed_scroll: Ref<ScrollViewer>,
    composer: Ref<TextBox>,
    last_rooms: RefCell<String>,
    last_messages: RefCell<String>,
    connect_button: Ref<Button>,
    approve: Ref<Button>,
    deny: Ref<Button>,
    send: Ref<Button>,
    forget: Ref<Button>,
    disconnect: Ref<Button>,
    join_voice: Ref<Button>,
    leave_voice: Ref<Button>,
    mute_mic: Ref<Button>,
    mute_speaker: Ref<Button>,
    share: Ref<Button>,
    share_source: Ref<Button>,
    share_quality: Ref<Button>,
    share_sources: RefCell<Vec<SourceInfo>>,
    share_source_index: Cell<usize>,
    share_preset_index: Cell<usize>,
    request_control: Ref<Button>,
    stop_control: Ref<Button>,
    allow_control: Ref<Button>,
    deny_control: Ref<Button>,
    control_status: Ref<TextBlock>,
    control_pending: Cell<bool>,
    control_peer: RefCell<Option<String>>,
    control_until: Cell<Option<Instant>>,
    control_host_peer: RefCell<Option<String>>,
    control_host_until: Cell<Option<Instant>>,
    control_request: RefCell<Option<(String, u64)>>,
    last_mouse: Cell<Instant>,
    input_tokens: RefCell<Vec<RoutedEventHandlerToken>>,
    control_line: RefCell<String>,
    file_members: Ref<StackPanel>,
    file_path: Ref<TextBox>,
    file_caption: Ref<TextBox>,
    file_send: Ref<Button>,
    file_choose: Ref<Button>,
    file_offer_panel: Ref<StackPanel>,
    file_offer_text: Ref<TextBlock>,
    file_accept: Ref<Button>,
    file_decline: Ref<Button>,
    file_status: Ref<TextBlock>,
    file_cancel: Ref<Button>,
    file_open: Ref<Button>,
    file_panel: Ref<StackPanel>,
    file_target: RefCell<Option<String>>,
    file_offers: RefCell<Vec<(String, String, String, u64)>>,
    file_names: RefCell<HashMap<String, String>>,
    file_active: RefCell<Option<String>>,
    file_last: RefCell<Option<PathBuf>>,
    file_members_sig: RefCell<String>,
    file_line: RefCell<String>,
    file_shown: RefCell<String>,
    update_status: Ref<TextBlock>,
    update_check: Ref<Button>,
    update_open: Ref<Button>,
    update_auto: Ref<CheckBox>,
    update_rx: RefCell<Option<Receiver<UpdateResult>>>,
    update_tx: Sender<UpdateResult>,
    update_url: RefCell<Option<String>>,
    presenter: Ref<TexturePresenter>,
    video_host: Ref<Grid>,
    surface: Rc<RefCell<VideoSurface>>,
    _presenter_sub: RefCell<Option<MediaSubscription>>,
    last_video: RefCell<Option<Instant>>,
    video_visible: Cell<bool>,
    video_label: Ref<TextBlock>,
    voice_status: Ref<TextBlock>,
    voice_note: RefCell<Option<String>>,
    voice_line: RefCell<String>,
    voice_wanted: Cell<bool>,
    voice_room_id: RefCell<Option<String>>,
    voice: RefCell<Option<VoiceRuntime>>,
    settings_screen: SettingsScreen,
    file_prefs: std::sync::Arc<FilePrefs>,
}

impl Ui {
    /// Copies the connect form into `settings` when the values are usable.
    fn apply_connection_form(&self, settings: &mut ClientSettings) {
        let host = self.host.text().unwrap_or_default().trim().to_owned();
        if !host.is_empty() {
            settings.host = host;
        }
        if let Ok(port) = self.port.text().unwrap_or_default().trim().parse::<u16>()
            && port > 0
        {
            settings.port = port;
        }
        settings.tls = self.tls.is_checked().unwrap_or(settings.tls);
        let name = self.name.text().unwrap_or_default().trim().to_owned();
        if !name.is_empty() {
            settings.name = name;
        }
        settings.remember_password = self
            .remember
            .is_checked()
            .unwrap_or(settings.remember_password);
    }

    /// Writes `settings` to disk, moving the remembered password when the user asked for it.
    fn persist(&self, settings: &ClientSettings) -> Result<(), String> {
        let password = self.password.text().unwrap_or_default();
        let remembered = settings.remember_password.then_some(password.as_str());
        self.memory.save(settings, remembered)
    }

    /// Saves what is typed in the connect panel, so a host or name survives a restart even when
    /// the user closes the window without connecting.
    fn save_connection_form(&self) {
        let mut settings = self.settings.borrow().clone();
        self.apply_connection_form(&mut settings);
        *self.settings.borrow_mut() = settings.clone();
        let _ = self.persist(&settings);
    }

    fn connect(&self) {
        let host = self.host.text().unwrap_or_default().trim().to_owned();
        let port = self
            .port
            .text()
            .unwrap_or_default()
            .trim()
            .parse::<u16>()
            .unwrap_or(0);
        let name = self.name.text().unwrap_or_default().trim().to_owned();
        let password = self.password.text().unwrap_or_default();
        if host.is_empty() || port == 0 {
            self.connect_status
                .set_text(Some("Enter a host and a port between 1 and 65535."));
            return;
        }
        if password.is_empty() {
            self.connect_status
                .set_text(Some("Enter the server password."));
            return;
        }
        let name = if name.is_empty() {
            "user".to_owned()
        } else {
            name
        };
        let tls = self.tls.is_checked().unwrap_or(true);
        let remember = self.remember.is_checked().unwrap_or(false);

        let mut settings = self.settings.borrow().clone();
        settings.host = host.clone();
        settings.port = port;
        settings.tls = tls;
        settings.name = name.clone();
        settings.remember_password = remember;
        self.settings.replace(settings.clone());
        if let Err(error) = self.persist(&settings) {
            self.connect_status
                .set_text(Some(&format!("Cannot save the connection: {error}")));
        }

        let _ = self.client.connect(ConnectRequest {
            host,
            port,
            tls,
            name,
            password,
            room: settings.last_room_id.clone(),
        });
    }

    fn send_message(&self) {
        let text = self.composer.text().unwrap_or_default();
        if text.trim().is_empty() {
            return;
        }
        if self.client.send_chat(text.trim()) {
            self.composer.set_text(Some(""));
        }
    }

    fn forget_key(&self) {
        let host = self.host.text().unwrap_or_default().trim().to_owned();
        let port = self
            .port
            .text()
            .unwrap_or_default()
            .trim()
            .parse::<u16>()
            .unwrap_or(0);
        if host.is_empty() || port == 0 {
            return;
        }
        let _ = self.client.forget_pin(host, port);
        self.connect_status.set_text(Some(
            "Pinned key forgotten. Reconnect to approve the server again.",
        ));
    }

    fn join_voice(self: &Rc<Self>) {
        if self.voice.borrow().is_some() {
            return;
        }
        let snapshot = self.client.snapshot();
        let Some(room) = snapshot.room.as_ref() else {
            return;
        };
        if !room.room.voice {
            *self.voice_note.borrow_mut() = Some("this room is text-only".to_owned());
            return;
        }
        let Some(self_id) = snapshot.self_id.clone() else {
            return;
        };
        let (mic, speaker) = {
            let settings = self.settings.borrow();
            (settings.mic_device.clone(), settings.speaker_device.clone())
        };
        let (audio, handle) = match AudioIo::start(mic.as_deref(), speaker.as_deref()) {
            Ok(value) => value,
            Err(error) => {
                *self.voice_note.borrow_mut() = Some(format!("cannot start audio: {error}"));
                return;
            }
        };
        let controller = voice::start(
            self.client.clone(),
            self_id,
            handle,
            &self.runtime,
            snapshot.turn.clone(),
            self.share_settings(),
            self.file_prefs.clone(),
        );
        *self.voice.borrow_mut() = Some(VoiceRuntime {
            audio,
            controller,
            mic_muted: Cell::new(true),
            speaker_muted: Cell::new(false),
            sharing: Cell::new(false),
            room: room.room.id.clone(),
            levels: RefCell::new(HashMap::new()),
            stats: RefCell::new(HashMap::new()),
            members_signature: RefCell::new(String::new()),
        });
        // Muted on join, like a push-to-talk call.
        let _ = self.client.set_voice(true);
        let _ = self.client.set_presence(Some(true), Some(false), None);
        *self.voice_note.borrow_mut() = Some("joining (microphone muted)".to_owned());
        self.voice_wanted.set(true);
        self.clear_control();
        *self.voice_room_id.borrow_mut() = Some(room.room.id.clone());
    }

    fn leave_voice(&self, reason: Option<&str>) {
        self.voice_wanted.set(false);
        self.clear_control();
        let taken = self.voice.borrow_mut().take();
        if let Some(mut runtime) = taken {
            runtime.controller.send(VoiceCommand::Stop);
            runtime.audio.stop();
            let _ = self.client.set_voice(false);
            let _ = self
                .client
                .set_presence(Some(false), Some(false), Some(false));
            *self.voice_note.borrow_mut() = Some(reason.unwrap_or("off").to_owned());
            self.show_video(false);
        }
    }

    fn toggle_mic(&self) {
        let muted = {
            let voice = self.voice.borrow();
            let Some(runtime) = voice.as_ref() else {
                return;
            };
            let muted = !runtime.mic_muted.get();
            runtime.mic_muted.set(muted);
            runtime.controller.send(VoiceCommand::MicMuted(muted));
            muted
        };
        *self.voice_note.borrow_mut() = Some(if muted {
            "microphone muted".to_owned()
        } else {
            "microphone live".to_owned()
        });
        self.mute_mic.set_content(Some(Rc::new(if muted {
            "Unmute mic".to_owned()
        } else {
            "Mute mic".to_owned()
        })));
    }

    fn toggle_speaker(&self) {
        let muted = {
            let voice = self.voice.borrow();
            let Some(runtime) = voice.as_ref() else {
                return;
            };
            let muted = !runtime.speaker_muted.get();
            runtime.speaker_muted.set(muted);
            runtime.controller.send(VoiceCommand::SpeakerMuted(muted));
            muted
        };
        *self.voice_note.borrow_mut() = Some(if muted {
            "speakers muted".to_owned()
        } else {
            "speakers live".to_owned()
        });
        self.mute_speaker.set_content(Some(Rc::new(if muted {
            "Unmute speakers".to_owned()
        } else {
            "Mute speakers".to_owned()
        })));
    }

    fn toggle_share(&self) {
        let sharing = {
            let voice = self.voice.borrow();
            let Some(runtime) = voice.as_ref() else {
                return;
            };
            let sharing = !runtime.sharing.get();
            runtime.sharing.set(sharing);
            runtime.controller.send(VoiceCommand::Share(sharing));
            sharing
        };
        *self.voice_note.borrow_mut() = Some(if sharing {
            "sharing this screen".to_owned()
        } else {
            "screen sharing stopped".to_owned()
        });
        self.share.set_content(Some(Rc::new(if sharing {
            "Stop sharing".to_owned()
        } else {
            "Share screen".to_owned()
        })));
        if !sharing {
            self.show_video(false);
        }
    }

    fn show_video(&self, visible: bool) {
        if self.video_visible.get() == visible {
            return;
        }
        self.video_visible.set(visible);
        self.presenter.set_is_visible(visible);
        self.video_host.set_is_visible(visible);
        self.video_label.set_is_visible(visible);
        if !visible {
            *self.last_video.borrow_mut() = None;
        }
    }

    fn controlling(&self) -> bool {
        self.control_peer.borrow().is_some()
            && self
                .control_until
                .get()
                .is_some_and(|until| Instant::now() < until)
    }

    fn member_name(&self, id: &str) -> String {
        self.client
            .snapshot()
            .room
            .as_ref()
            .and_then(|room| {
                room.members
                    .iter()
                    .find(|member| member.session_id == id)
                    .map(|member| member.name.clone())
            })
            .unwrap_or_else(|| id.to_owned())
    }

    fn send_voice_command(&self, command: VoiceCommand) {
        if let Some(runtime) = self.voice.borrow().as_ref() {
            runtime.controller.send(command);
        }
    }

    fn share_settings(&self) -> ShareSettings {
        let preset = SHARE_PRESETS[self.share_preset_index.get() % SHARE_PRESETS.len()];
        let source = self
            .share_sources
            .borrow()
            .get(self.share_source_index.get())
            .map(|info| info.source.clone())
            .unwrap_or(CaptureSource::Monitor(1));
        ShareSettings {
            source,
            max_width: preset.0,
            max_height: preset.1,
            fps: preset.2,
            bitrate: preset.3,
        }
    }

    fn cycle_share_source(&self) {
        let count = self.share_sources.borrow().len();
        if count == 0 {
            return;
        }
        let index = (self.share_source_index.get() + 1) % count;
        self.share_source_index.set(index);
        if let Some(info) = self.share_sources.borrow().get(index) {
            self.share_source
                .set_content(Some(Rc::new(format!("Source: {}", info.label))));
        }
        self.persist_share();
    }

    fn cycle_share_quality(&self) {
        let index = (self.share_preset_index.get() + 1) % SHARE_PRESETS.len();
        self.share_preset_index.set(index);
        self.share_quality.set_content(Some(Rc::new(format!(
            "Quality: {}",
            preset_label(SHARE_PRESETS[index])
        ))));
        self.persist_share();
    }

    fn persist_share(&self) {
        let share = self.share_settings();
        let mut settings = self.settings.borrow().clone();
        settings.share_source = Some(share.source.id());
        settings.share_width = share.max_width;
        settings.share_height = share.max_height;
        settings.share_fps = share.fps;
        settings.share_bitrate = share.bitrate;
        *self.settings.borrow_mut() = settings.clone();
        let _ = self.memory.save_settings(&settings);
        if self.voice.borrow().is_some() {
            self.send_voice_command(VoiceCommand::ConfigureShare(share));
        }
    }

    fn check_updates(&self) {
        self.update_status.set_text(Some("checking\u{2026}"));
        let tx = self.update_tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(update::check(APP_VERSION));
        });
    }

    fn open_release(&self) {
        if let Some(url) = self.update_url.borrow().clone() {
            let _ = std::process::Command::new("cmd")
                .args(["/C", "start", "", &url])
                .spawn();
        }
    }

    fn set_update_auto(&self, checked: bool) {
        let mut settings = self.settings.borrow().clone();
        settings.check_for_updates = checked;
        *self.settings.borrow_mut() = settings.clone();
        let _ = self.memory.save_settings(&settings);
    }

    fn set_settings_visible(&self, open: bool) {
        self.settings_screen.visible.set(open);
        self.settings_screen.view.set_is_visible(open);
        if open {
            self.select_settings_section(self.settings_screen.section.get());
            self.refresh_settings();
        }
    }

    /// Shows one settings page and updates the header title.
    fn select_settings_section(&self, index: usize) {
        let screen = &self.settings_screen;
        let index = index.min(screen.sections.len().saturating_sub(1));
        screen.section.set(index);
        for (position, section) in screen.sections.iter().enumerate() {
            section.set_is_visible(position == index);
        }
    }

    /// Applies a change immediately: persisted to disk and pushed to the parts that hold a copy.
    fn change_settings(&self, change: impl FnOnce(&mut ClientSettings)) {
        let mut settings = self.settings.borrow().clone();
        // The connect panel is the source of truth for the endpoint and display name.
        self.apply_connection_form(&mut settings);
        change(&mut settings);
        *self.settings.borrow_mut() = settings.clone();
        match self.persist(&settings) {
            Ok(()) => self.settings_screen.status.set_text(Some("Saved.")),
            Err(error) => self
                .settings_screen
                .status
                .set_text(Some(&format!("Cannot save: {error}"))),
        }
        self.client.set_keep_history(settings.keep_history);
        self.file_prefs
            .set_open_when_finished(settings.open_when_finished);
        self.file_prefs
            .set_downloads(settings.downloads_dir.as_ref().map(PathBuf::from));
        self.refresh_settings();
    }

    fn refresh_settings(&self) {
        let settings = self.settings.borrow().clone();
        let screen = &self.settings_screen;
        screen
            .auto_connect
            .set_is_checked(Some(settings.auto_connect));
        screen
            .timestamps
            .set_is_checked(Some(settings.show_timestamps));
        screen.clock24.set_is_checked(Some(settings.clock_24h));
        screen
            .keep_history
            .set_is_checked(Some(settings.keep_history));
        screen
            .open_when_finished
            .set_is_checked(Some(settings.open_when_finished));

        let label = |choices: &[DeviceChoice], index: usize| {
            choices
                .get(index)
                .map(|choice| choice.label.clone())
                .unwrap_or_else(|| "System default".to_owned())
        };
        screen.mic.set_content(Some(Rc::new(format!(
            "Microphone: {}",
            label(&screen.inputs.borrow(), screen.input_index.get())
        ))));
        screen.speaker.set_content(Some(Rc::new(format!(
            "Speakers: {}",
            label(&screen.outputs.borrow(), screen.output_index.get())
        ))));
        screen.quality.set_content(Some(Rc::new(format!(
            "Default quality: {}",
            preset_label(SHARE_PRESETS[self.share_preset_index.get() % SHARE_PRESETS.len()])
        ))));

        let downloads = match &settings.downloads_dir {
            Some(dir) => dir.clone(),
            None => rust_chat_desktop::files::download_dir()
                .map(|dir| format!("{} (default)", dir.display()))
                .unwrap_or_else(|error| error),
        };
        screen
            .downloads
            .set_text(Some(&format!("Received files go to {downloads}")));
        screen
            .about_version
            .set_text(Some(&format!("rust-chat {APP_VERSION}")));
        screen.about_data.set_text(Some(&format!(
            "Settings and history: {}",
            rust_chat_client_core::settings::app_dir().display()
        )));
    }

    fn cycle_device(&self, input: bool) {
        let screen = &self.settings_screen;
        let (choices, index) = if input {
            (screen.inputs.borrow(), &screen.input_index)
        } else {
            (screen.outputs.borrow(), &screen.output_index)
        };
        if choices.is_empty() {
            return;
        }
        let next = (index.get() + 1) % choices.len();
        index.set(next);
        let id = choices.get(next).and_then(|choice| choice.id.clone());
        drop(choices);
        if input {
            self.change_settings(|settings| settings.mic_device = id);
        } else {
            self.change_settings(|settings| settings.speaker_device = id);
        }
    }

    fn cycle_settings_quality(&self) {
        let index = (self.share_preset_index.get() + 1) % SHARE_PRESETS.len();
        self.share_preset_index.set(index);
        self.share_quality.set_content(Some(Rc::new(format!(
            "Quality: {}",
            preset_label(SHARE_PRESETS[index])
        ))));
        let preset = SHARE_PRESETS[index];
        self.change_settings(|settings| {
            settings.share_width = preset.0;
            settings.share_height = preset.1;
            settings.share_fps = preset.2;
            settings.share_bitrate = preset.3;
        });
        self.persist_share();
    }

    fn choose_downloads(&self) {
        let Some(dir) = rfd::FileDialog::new().pick_folder() else {
            return;
        };
        match rust_chat_desktop::files::ensure_download_dir(&dir) {
            Ok(dir) => {
                let text = dir.display().to_string();
                self.change_settings(|settings| settings.downloads_dir = Some(text));
            }
            Err(error) => self.settings_screen.status.set_text(Some(&error)),
        }
    }

    fn reset_downloads(&self) {
        self.change_settings(|settings| settings.downloads_dir = None);
    }

    /// Arms a destructive action so it takes a second click.
    fn arm_confirm(&self, action: ConfirmAction) {
        let prompt = match action {
            ConfirmAction::ForgetPin => {
                "Forget the pinned server key? The next connection needs a new approval."
            }
            ConfirmAction::ClearHistory => "Delete the saved transcript of this room from disk?",
        };
        *self.settings_screen.pending.borrow_mut() = Some(action);
        self.settings_screen.confirm_text.set_text(Some(prompt));
        self.settings_screen.danger_bar.set_is_visible(true);
        self.settings_screen.confirm.set_is_visible(true);
        self.settings_screen.cancel.set_is_visible(true);
    }

    fn cancel_confirm(&self) {
        *self.settings_screen.pending.borrow_mut() = None;
        self.settings_screen.confirm_text.set_text(Some(""));
        self.settings_screen.confirm.set_is_visible(false);
        self.settings_screen.cancel.set_is_visible(false);
        self.settings_screen.danger_bar.set_is_visible(false);
    }

    fn run_confirm(&self) {
        let Some(action) = self.settings_screen.pending.borrow_mut().take() else {
            return;
        };
        self.cancel_confirm();
        match action {
            ConfirmAction::ForgetPin => {
                let (host, port) = {
                    let settings = self.settings.borrow();
                    (settings.host.clone(), settings.port)
                };
                let _ = self.client.forget_pin(host, port);
                self.settings_screen
                    .status
                    .set_text(Some("Pinned key forgotten."));
            }
            ConfirmAction::ClearHistory => self.clear_local_history(),
        }
    }

    fn clear_local_history(&self) {
        let (endpoint, room) = {
            let settings = self.settings.borrow();
            let room = self
                .client
                .snapshot()
                .room
                .as_ref()
                .map(|room| room.room.id.clone())
                .or_else(|| settings.last_room_id.clone());
            (format!("{}:{}", settings.host, settings.port), room)
        };
        let Some(room) = room else {
            self.settings_screen
                .status
                .set_text(Some("Join a room first."));
            return;
        };
        let store = rust_chat_client_core::history::HistoryStore::new();
        let result = store.clear(&endpoint, &room);
        self.client.clear_messages();
        match result {
            Ok(()) => self
                .settings_screen
                .status
                .set_text(Some(&format!("Cleared the saved history of {room}."))),
            Err(error) => self
                .settings_screen
                .status
                .set_text(Some(&format!("Cannot clear history: {error}"))),
        }
    }

    fn request_control(&self) {
        let snapshot = self.client.snapshot();
        let Some(room) = snapshot.room.as_ref() else {
            return;
        };
        let Some(presenter) = room
            .members
            .iter()
            .find(|member| member.sharing && member.session_id != room.self_member.session_id)
        else {
            *self.voice_note.borrow_mut() = Some("nobody is sharing".to_owned());
            return;
        };
        self.send_voice_command(VoiceCommand::ControlRequest {
            peer: presenter.session_id.clone(),
            lease_secs: DEFAULT_LEASE_SECS,
        });
        self.control_pending.set(true);
        *self.voice_note.borrow_mut() =
            Some(format!("waiting for {} to allow control", presenter.name));
    }

    fn respond_control(&self, allow: bool) {
        let Some((peer, lease_secs)) = self.control_request.borrow_mut().take() else {
            return;
        };
        self.send_voice_command(VoiceCommand::ControlRespond {
            peer,
            allow,
            lease_secs,
        });
        if !allow {
            *self.voice_note.borrow_mut() = Some("control denied".to_owned());
        }
    }

    fn stop_control(&self) {
        self.send_voice_command(VoiceCommand::ControlStop);
        self.clear_control();
        *self.voice_note.borrow_mut() = Some("control stopped".to_owned());
    }

    fn clear_control(&self) {
        self.control_pending.set(false);
        *self.control_peer.borrow_mut() = None;
        self.control_until.set(None);
        *self.control_host_peer.borrow_mut() = None;
        self.control_host_until.set(None);
        *self.control_request.borrow_mut() = None;
    }

    fn choose_file(&self) {
        if let Some(path) = rfd::FileDialog::new().pick_file() {
            self.file_path.set_text(Some(&path.display().to_string()));
        }
    }

    fn send_file(&self) {
        let Some(target) = self.file_target.borrow().clone() else {
            *self.file_line.borrow_mut() = "choose a member to send to".to_owned();
            return;
        };
        let path = self.file_path.text().unwrap_or_default().trim().to_owned();
        if path.is_empty() {
            *self.file_line.borrow_mut() = "choose a file first".to_owned();
            return;
        }
        let caption = self.file_caption.text().unwrap_or_default();
        self.send_voice_command(VoiceCommand::SendFile {
            peer: target,
            path,
            caption,
        });
    }

    fn accept_offer(&self) {
        let offer = {
            let mut offers = self.file_offers.borrow_mut();
            if offers.is_empty() {
                return;
            }
            offers.remove(0)
        };
        let (id, peer, name, _) = offer;
        self.send_voice_command(VoiceCommand::AcceptFile { id: id.clone() });
        *self.file_active.borrow_mut() = Some(id);
        *self.file_line.borrow_mut() = format!("receiving {name} from {}", self.member_name(&peer));
        self.update_offer_panel();
    }

    fn decline_offer(&self) {
        let offer = {
            let mut offers = self.file_offers.borrow_mut();
            if offers.is_empty() {
                return;
            }
            offers.remove(0)
        };
        self.send_voice_command(VoiceCommand::DeclineFile { id: offer.0 });
        *self.file_line.borrow_mut() = "declined".to_owned();
        self.update_offer_panel();
    }

    fn cancel_file(&self) {
        let Some(id) = self.file_active.borrow().clone() else {
            return;
        };
        self.send_voice_command(VoiceCommand::CancelFile { id });
        *self.file_active.borrow_mut() = None;
        *self.file_line.borrow_mut() = "cancelled".to_owned();
    }

    fn open_last_file(&self) {
        let path = self.file_last.borrow().clone();
        if let Some(path) = path {
            open_path(&path.display().to_string());
        }
    }

    fn update_offer_panel(&self) {
        let offers = self.file_offers.borrow();
        let Some((_, peer, name, size)) = offers.first() else {
            self.file_offer_panel.set_is_visible(false);
            return;
        };
        self.file_offer_text.set_text(Some(&format!(
            "{} offers {} ({})",
            self.member_name(peer),
            name,
            rust_chat_desktop::files::human_size(*size)
        )));
        self.file_offer_panel.set_is_visible(true);
    }

    fn refresh_file_members(self: &Rc<Self>, room: &rust_chat_protocol::RoomState) {
        let signature = room
            .members
            .iter()
            .filter(|member| member.session_id != room.self_member.session_id)
            .map(|member| member.session_id.clone())
            .collect::<Vec<_>>()
            .join(",");
        if *self.file_members_sig.borrow() == signature {
            return;
        }
        *self.file_members_sig.borrow_mut() = signature.clone();
        self.file_members.children().clear();
        let mut first = None;
        for member in &room.members {
            if member.session_id == room.self_member.session_id {
                continue;
            }
            if first.is_none() {
                first = Some(member.session_id.clone());
            }
            let button = Button::new();
            button.set_content(Some(Rc::new(format!("To {}", member.name))));
            let weak = Rc::downgrade(self);
            let id = member.session_id.clone();
            let label = member.name.clone();
            button.click(move |_, _| {
                if let Some(ui) = weak.upgrade() {
                    *ui.file_target.borrow_mut() = Some(id.clone());
                    *ui.file_line.borrow_mut() = format!("send to {label}");
                }
            });
            self.file_members.children().add(button);
        }
        let valid = self
            .file_target
            .borrow()
            .as_ref()
            .is_some_and(|target| signature.split(',').any(|id| id == target));
        if !valid {
            *self.file_target.borrow_mut() = first;
        }
    }

    fn send_input(&self, event: InputEvent) {
        if !self.controlling() {
            return;
        }
        self.send_voice_command(VoiceCommand::ControlInput(event));
    }

    fn on_pointer_moved(&self, e: &PointerEventArgs) {
        if !self.controlling() {
            return;
        }
        if self.last_mouse.get().elapsed() < Duration::from_millis(16) {
            return;
        }
        self.last_mouse.set(Instant::now());
        let point = e.get_position(Some(&*self.video_host));
        if std::env::var("RUST_CHAT_CONTROL_DEBUG").is_ok() {
            eprintln!("pointer move {:?}", (point.x, point.y));
        }
        let x = (point.x / PRESENTER_WIDTH).clamp(0.0, 1.0) as f32;
        let y = (point.y / PRESENTER_HEIGHT).clamp(0.0, 1.0) as f32;
        self.send_input(InputEvent::MouseMove { x, y });
    }

    fn on_pointer_pressed(&self, e: &PointerPressedEventArgs) {
        if !self.controlling() {
            return;
        }
        self.presenter.focus();
        let properties = e.get_current_point(None).properties;
        let button = if properties.is_left_button_pressed {
            Some(RemoteButton::Left)
        } else if properties.is_right_button_pressed {
            Some(RemoteButton::Right)
        } else if properties.is_middle_button_pressed {
            Some(RemoteButton::Middle)
        } else {
            None
        };
        let Some(button) = button else {
            return;
        };
        self.send_input(InputEvent::MouseButton {
            button,
            pressed: true,
        });
        e.set_handled(true);
    }

    fn on_pointer_released(&self, e: &PointerReleasedEventArgs) {
        if !self.controlling() {
            return;
        }
        let Some(button) = remote_button(e.initial_press_mouse_button()) else {
            return;
        };
        self.send_input(InputEvent::MouseButton {
            button,
            pressed: false,
        });
        e.set_handled(true);
    }

    fn on_pointer_wheel(&self, e: &PointerWheelEventArgs) {
        if !self.controlling() {
            return;
        }
        let delta = e.delta();
        if delta.y != 0.0 {
            self.send_input(InputEvent::Wheel {
                delta: (delta.y * 120.0) as i32,
                horizontal: false,
            });
        }
        if delta.x != 0.0 {
            self.send_input(InputEvent::Wheel {
                delta: (delta.x * 120.0) as i32,
                horizontal: true,
            });
        }
        e.set_handled(true);
    }

    fn on_key(&self, e: &KeyEventArgs, pressed: bool) {
        if !self.controlling() {
            return;
        }
        let Some(vk) = virtual_key(e.key, e.key_symbol.as_deref()) else {
            return;
        };
        self.send_input(InputEvent::Key { vk, pressed });
        e.set_handled(true);
    }
}

/// A destructive action that needs a second click.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ConfirmAction {
    ForgetPin,
    ClearHistory,
}

/// Controls of the settings screen. Everything is persisted on change.
struct SettingsScreen {
    view: Ref<Grid>,
    nav: Vec<Ref<Button>>,
    sections: Vec<Ref<StackPanel>>,
    section: Cell<usize>,
    danger_bar: Ref<Border>,
    open_room: Ref<Button>,
    open_connect: Ref<Button>,
    close: Ref<Button>,
    status: Ref<TextBlock>,
    auto_connect: Ref<CheckBox>,
    timestamps: Ref<CheckBox>,
    clock24: Ref<CheckBox>,
    keep_history: Ref<CheckBox>,
    open_when_finished: Ref<CheckBox>,
    mic: Ref<Button>,
    speaker: Ref<Button>,
    quality: Ref<Button>,
    downloads: Ref<TextBlock>,
    choose_downloads: Ref<Button>,
    reset_downloads: Ref<Button>,
    about_version: Ref<TextBlock>,
    about_data: Ref<TextBlock>,
    forget_pin: Ref<Button>,
    clear_history: Ref<Button>,
    pending: RefCell<Option<ConfirmAction>>,
    confirm_text: Ref<TextBlock>,
    confirm: Ref<Button>,
    cancel: Ref<Button>,
    inputs: RefCell<Vec<DeviceChoice>>,
    outputs: RefCell<Vec<DeviceChoice>>,
    input_index: Cell<usize>,
    output_index: Cell<usize>,
    visible: Cell<bool>,
}

fn small_button(label: &str) -> Ref<Button> {
    let button = Button::new();
    button.set_content(Some(Rc::new(label.to_owned())));
    button
}

fn build_settings_screen(settings: &ClientSettings) -> SettingsScreen {
    let devices = rust_chat_media_desktop::audio::audio_devices();
    let inputs = device_choices(devices.inputs);
    let outputs = device_choices(devices.outputs);
    let input_index = device_index(&inputs, settings.mic_device.as_deref());
    let output_index = device_index(&outputs, settings.speaker_device.as_deref());

    let open_room = icons::icon_button(icons::Icon::Settings, "Settings");
    let open_connect = icons::icon_button(icons::Icon::Settings, "Settings");
    let close = icons::icon_button(icons::Icon::Close, "Close settings");
    let status = ui_kit::muted("", 12.0);

    // --- controls ---
    let auto_connect = CheckBox::new();
    auto_connect.set_is_checked(Some(settings.auto_connect));
    let forget_pin = icons::icon_button(icons::Icon::Shield, "Forget pinned key");

    let timestamps = CheckBox::new();
    timestamps.set_is_checked(Some(settings.show_timestamps));
    let clock24 = CheckBox::new();
    clock24.set_is_checked(Some(settings.clock_24h));
    let keep_history = CheckBox::new();
    keep_history.set_is_checked(Some(settings.keep_history));
    let clear_history = icons::icon_button(icons::Icon::Delete, "Clear local history");

    let mic = small_button("Microphone: system default");
    let speaker = small_button("Speakers: system default");
    let quality = small_button("Default quality");

    let downloads = ui_kit::muted("", 12.0);
    let choose_downloads = icons::icon_button(icons::Icon::Folder, "Choose the downloads folder");
    let reset_downloads =
        icons::icon_button(icons::Icon::Restart, "Use the default Downloads folder");
    let downloads_buttons = ui_kit::row(6.0);
    downloads_buttons.children().add(&choose_downloads);
    downloads_buttons.children().add(&reset_downloads);
    let open_when_finished = CheckBox::new();
    open_when_finished.set_is_checked(Some(settings.open_when_finished));

    let about_version = ui_kit::muted("", 12.0);
    let about_data = ui_kit::muted("", 12.0);

    // --- sections ---
    let mut sections: Vec<Ref<StackPanel>> = Vec::new();
    let mut nav: Vec<Ref<Button>> = Vec::new();

    let connection = settings_section(
        icons::Icon::Server,
        "Connection",
        "chat.example.org:8444 · TLS · your key is approved once",
        vec![
            settings_row(
                "Connect automatically on start",
                Some("Uses the password from Credential Manager"),
                &auto_connect,
            ),
            settings_row(
                "Pinned server key",
                Some("Clients approve the certificate on first connect"),
                &forget_pin,
            ),
        ],
    );
    let chat = settings_section(
        icons::Icon::Chat,
        "Chat",
        "Applies immediately, mid-conversation.",
        vec![
            settings_row(
                "Show timestamps",
                Some("Local time next to each message"),
                &timestamps,
            ),
            settings_row(
                "24-hour clock",
                Some("Otherwise 12-hour with AM/PM"),
                &clock24,
            ),
            settings_row(
                "Keep history on disk",
                Some("Plain JSONL, one file per server and room"),
                &keep_history,
            ),
            settings_row(
                "Local history",
                Some("Deletes the saved transcript of the current room"),
                &clear_history,
            ),
        ],
    );
    let audio = settings_section(
        icons::Icon::Microphone,
        "Audio & video",
        "Devices and the default share quality.",
        vec![
            settings_row(
                "Microphone",
                Some("System default or a specific input device"),
                &mic,
            ),
            settings_row("Speakers", Some("Where incoming voice is played"), &speaker),
            settings_row(
                "Screen share quality",
                Some("Resolution, frame rate and target bitrate"),
                &quality,
            ),
        ],
    );
    let files = settings_section(
        icons::Icon::Folder,
        "Files",
        "Received files and what happens when a transfer completes.",
        vec![
            settings_row("Download folder", None, &downloads),
            settings_row(
                "Change folder",
                Some("Pick another folder or return to the OS default"),
                &downloads_buttons,
            ),
            settings_row(
                "Open received files when finished",
                Some("Only after the SHA-256 matches"),
                &open_when_finished,
            ),
        ],
    );
    let about = settings_section(
        icons::Icon::Info,
        "About",
        "Version, update checks and where the app keeps its data.",
        vec![
            settings_row("Version", None, &about_version),
            settings_row("Settings and history", None, &about_data),
        ],
    );
    for (glyph, title, panel) in [
        (icons::Icon::Server, "Connection", &connection),
        (icons::Icon::Chat, "Chat", &chat),
        (icons::Icon::Microphone, "Audio & video", &audio),
        (icons::Icon::Folder, "Files", &files),
        (icons::Icon::Info, "About", &about),
    ] {
        let content = icons::icon_label(glyph, title);
        let button = Button::new();
        button.set_background(Some(ui_kit::brush(ui_kit::RAIL)));
        button.set_content(Some(Rc::new(content) as ferroui_base::BoxedValue));
        button.set_horizontal_content_alignment(HorizontalAlignment::Left);
        button.set_horizontal_alignment(HorizontalAlignment::Stretch);
        nav.push(button);
        sections.push(panel.clone());
    }

    // --- danger bar + footer ---
    let confirm_text = ui_kit::body("", 12.5);
    let confirm = icons::icon_button(icons::Icon::Check, "Confirm");
    let cancel = icons::icon_button(icons::Icon::Close, "Cancel");
    confirm.set_is_visible(false);
    cancel.set_is_visible(false);
    let danger_row = ui_kit::row(8.0);
    danger_row.children().add(&confirm_text);
    danger_row.children().add(&confirm);
    danger_row.children().add(&cancel);
    let danger_bar = Border::new();
    danger_bar.set_background(Some(ui_kit::brush(0xFFFD_F3F2)));
    danger_bar.set_border_brush(Some(ui_kit::brush(0xFFF0_C9C4)));
    danger_bar.set_border_thickness(Thickness::uniform(1.0));
    danger_bar.set_corner_radius(CornerRadius::uniform(8.0));
    danger_bar.set_padding(Thickness {
        left: 12.0,
        top: 8.0,
        right: 12.0,
        bottom: 8.0,
    });
    danger_bar.set_child(&danger_row);
    danger_bar.set_horizontal_alignment(HorizontalAlignment::Stretch);
    danger_bar.set_is_visible(false);
    danger_bar.set_margin(Thickness {
        left: 0.0,
        top: 4.0,
        right: 0.0,
        bottom: 0.0,
    });

    // --- body: one section visible at a time, inside a scroller ---
    let body = StackPanel::new();
    body.set_spacing(10.0);
    for section in &sections {
        section.set_is_visible(false);
        body.children().add(section);
    }
    body.children().add(&danger_bar);
    let scroller = ScrollViewer::new();
    scroller.set_vertical_scroll_bar_visibility(ScrollBarVisibility::Auto);
    scroller.set_horizontal_scroll_bar_visibility(ScrollBarVisibility::Disabled);
    let body_frame = Border::new();
    body_frame.set_padding(Thickness {
        left: 20.0,
        top: 16.0,
        right: 20.0,
        bottom: 16.0,
    });
    body_frame.set_child(&body);
    scroller.set_content(Some(Control::boxed(&body_frame)));

    // --- header, nav, footer ---
    let heading = ui_kit::headline("Settings");
    let header = ui_kit::row(8.0);
    header.children().add(&heading);
    let header_frame = Border::new();
    header_frame.set_background(Some(ui_kit::brush(ui_kit::CARD)));
    header_frame.set_padding(Thickness {
        left: 20.0,
        top: 10.0,
        right: 14.0,
        bottom: 10.0,
    });
    header_frame.set_border_brush(Some(ui_kit::brush(ui_kit::STROKE)));
    header_frame.set_border_thickness(Thickness {
        left: 0.0,
        top: 0.0,
        right: 0.0,
        bottom: 1.0,
    });
    header_frame.set_child(&header);

    let nav_panel = StackPanel::new();
    nav_panel.set_spacing(2.0);
    nav_panel.children().add(ui_kit::section_label("SETTINGS"));
    for button in &nav {
        nav_panel.children().add(button);
    }
    nav_panel.children().add(ui_kit::divider());
    nav_panel.children().add(&close);
    let nav_frame = Border::new();
    nav_frame.set_background(Some(ui_kit::brush(ui_kit::RAIL)));
    nav_frame.set_padding(Thickness {
        left: 10.0,
        top: 14.0,
        right: 10.0,
        bottom: 14.0,
    });
    nav_frame.set_border_brush(Some(ui_kit::brush(ui_kit::STROKE)));
    nav_frame.set_border_thickness(Thickness {
        left: 0.0,
        top: 0.0,
        right: 1.0,
        bottom: 0.0,
    });
    nav_frame.set_child(&nav_panel);

    let footer = ui_kit::row(10.0);
    footer.children().add(&status);
    let footer_frame = Border::new();
    footer_frame.set_background(Some(ui_kit::brush(ui_kit::CARD)));
    footer_frame.set_padding(Thickness {
        left: 20.0,
        top: 8.0,
        right: 20.0,
        bottom: 8.0,
    });
    footer_frame.set_border_brush(Some(ui_kit::brush(ui_kit::STROKE)));
    footer_frame.set_border_thickness(Thickness {
        left: 0.0,
        top: 1.0,
        right: 0.0,
        bottom: 0.0,
    });
    footer_frame.set_child(&footer);

    let content = Grid::new();
    content.set_row_definitions(RowDefinitions::from_items([
        RowDefinition::with_height(GridLength::AUTO),
        RowDefinition::with_height(GridLength::new(1.0, GridUnitType::Star)),
        RowDefinition::with_height(GridLength::AUTO),
    ]));
    Grid::set_row(&header_frame, 0);
    Grid::set_row(&scroller, 1);
    Grid::set_row(&footer_frame, 2);
    content.children().add(&header_frame);
    content.children().add(&scroller);
    content.children().add(&footer_frame);

    let view = Grid::new();
    view.set_column_definitions(ColumnDefinitions::from_items([
        ColumnDefinition::with_width(GridLength::new(210.0, GridUnitType::Pixel)),
        ColumnDefinition::with_width(GridLength::new(1.0, GridUnitType::Star)),
    ]));
    Grid::set_column(&nav_frame, 0);
    Grid::set_column(&content, 1);
    view.children().add(&nav_frame);
    view.children().add(&content);
    view.set_is_visible(false);

    SettingsScreen {
        view,
        nav,
        sections,
        section: Cell::new(0),
        danger_bar: danger_bar.clone(),
        open_room,
        open_connect,
        close,
        status,
        auto_connect,
        timestamps,
        clock24,
        keep_history,
        open_when_finished,
        mic,
        speaker,
        quality,
        downloads,
        choose_downloads,
        reset_downloads,
        about_version,
        about_data,
        forget_pin,
        clear_history,
        pending: RefCell::new(None),
        confirm_text,
        confirm,
        cancel,
        inputs: RefCell::new(inputs),
        outputs: RefCell::new(outputs),
        input_index: Cell::new(input_index),
        output_index: Cell::new(output_index),
        visible: Cell::new(false),
    }
}

/// A label + hint on the left, a control on the right, with the padding of a settings row.
fn settings_row<T>(label: &str, hint: Option<&str>, control: &Ref<T>) -> Ref<Border>
where
    T: ferroui_base::ObjectType + ferroui_base::Upcast<Control>,
{
    let text_column = StackPanel::new();
    text_column.set_spacing(2.0);
    text_column.children().add(ui_kit::body(label, 13.5));
    if let Some(hint) = hint {
        text_column.children().add(ui_kit::muted(hint, 11.5));
    }
    let row = Grid::new();
    row.set_column_definitions(ColumnDefinitions::from_items([
        ColumnDefinition::with_width(GridLength::new(1.0, GridUnitType::Star)),
        ColumnDefinition::with_width(GridLength::AUTO),
    ]));
    Grid::set_column(&text_column, 0);
    let control: Ref<Control> = control.clone().upcast::<Control>();
    Grid::set_column(&control, 1);
    row.children().add(&text_column);
    row.children().add(control);
    let border = Border::new();
    border.set_padding(Thickness {
        left: 14.0,
        top: 11.0,
        right: 14.0,
        bottom: 11.0,
    });
    border.set_child(&row);
    border
}

/// One settings page: a headline, a one-line explanation and a card of rows.
fn settings_section(
    glyph: icons::Icon,
    title: &str,
    subtitle: &str,
    rows: Vec<Ref<Border>>,
) -> Ref<StackPanel> {
    let heading = ui_kit::row(8.0);
    heading.children().add(icons::icon(glyph, 16.0));
    heading.children().add(ui_kit::headline(title));
    let column = StackPanel::new();
    column.set_spacing(2.0);
    column.children().add(&heading);
    column.children().add(ui_kit::muted(subtitle, 12.0));

    let rows_panel = StackPanel::new();
    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            rows_panel.children().add(ui_kit::divider());
        }
        rows_panel.children().add(row);
    }
    let card = ui_kit::card(&rows_panel, 0.0);

    let section = StackPanel::new();
    section.set_spacing(10.0);
    section.children().add(&column);
    section.children().add(&card);
    section
}

/// The desktop work area in physical pixels.
#[derive(Clone, Copy)]
struct WorkArea {
    left: i32,
    top: i32,
    width: u32,
    height: u32,
}

fn work_area() -> Option<WorkArea> {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::{SPI_GETWORKAREA, SystemParametersInfoW};
    let mut rect = RECT::default();
    // SAFETY: the call only writes the RECT that was passed by pointer.
    let result = unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut rect as *mut RECT as *mut std::ffi::c_void),
            Default::default(),
        )
    };
    result.is_ok().then(|| WorkArea {
        left: rect.left,
        top: rect.top,
        width: (rect.right - rect.left).max(0) as u32,
        height: (rect.bottom - rect.top).max(0) as u32,
    })
}

/// Physical pixels per logical pixel for the system DPI.
fn system_scale() -> f64 {
    use windows::Win32::UI::HiDpi::GetDpiForSystem;
    let dpi = unsafe { GetDpiForSystem() } as f64;
    if dpi > 0.0 { dpi / 96.0 } else { 1.0 }
}

/// Opens a file or URL with its default handler.
fn open_path(target: &str) {
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", target])
        .spawn();
}

fn remote_button(button: ferroui_base::input::MouseButton) -> Option<RemoteButton> {
    match button {
        ferroui_base::input::MouseButton::Left => Some(RemoteButton::Left),
        ferroui_base::input::MouseButton::Right => Some(RemoteButton::Right),
        ferroui_base::input::MouseButton::Middle => Some(RemoteButton::Middle),
        _ => None,
    }
}

/// WPF-style `Key` values to Windows virtual-key codes, with `VkKeyScanW` for printable keys.
fn virtual_key(key: Key, symbol: Option<&str>) -> Option<u16> {
    if let Some(ch) = symbol.and_then(|symbol| symbol.chars().next())
        && ch.is_ascii()
        && !ch.is_control()
    {
        let scan = unsafe { windows::Win32::UI::Input::KeyboardAndMouse::VkKeyScanW(ch as u16) };
        if scan != -1 {
            return Some((scan & 0xFF) as u16);
        }
    }
    let value = key as u32;
    let vk = if (44..=69).contains(&value) {
        0x41 + (value - 44) as u16
    } else if (34..=43).contains(&value) {
        0x30 + (value - 34) as u16
    } else if (74..=83).contains(&value) {
        0x60 + (value - 74) as u16
    } else if (90..=101).contains(&value) {
        0x70 + (value - 90) as u16
    } else {
        match key {
            Key::Escape => 0x1B,
            Key::Tab => 0x09,
            Key::Back => 0x08,
            Key::Return => 0x0D,
            Key::Space => 0x20,
            Key::Insert => 0x2D,
            Key::Delete => 0x2E,
            Key::Home => 0x24,
            Key::End => 0x23,
            Key::PageUp => 0x21,
            Key::PageDown => 0x22,
            Key::Left => 0x25,
            Key::Up => 0x26,
            Key::Right => 0x27,
            Key::Down => 0x28,
            Key::LeftShift => 0xA0,
            Key::RightShift => 0xA1,
            Key::LeftCtrl => 0xA2,
            Key::RightCtrl => 0xA3,
            Key::LeftAlt => 0xA4,
            Key::RightAlt => 0xA5,
            Key::LWin => 0x5B,
            Key::RWin => 0x5C,
            Key::CapsLock => 0x14,
            Key::Pause => 0x13,
            _ => return None,
        }
    };
    Some(vk)
}

fn create_main_window() -> Ref<Window> {
    let Some(startup) = STARTUP.with(|cell| cell.borrow_mut().take()) else {
        panic!("the client was not initialized");
    };
    let settings = startup.settings.clone();
    let password = startup.password.clone();
    let settings_screen = build_settings_screen(&settings);

    let host = TextBox::new();
    host.set_text(Some(&settings.host));
    host.set_width(280.0);
    let port = TextBox::new();
    port.set_text(Some(&settings.port.to_string()));
    port.set_width(100.0);
    let name = TextBox::new();
    name.set_text(Some(&settings.name));
    name.set_width(220.0);
    let password_box = TextBox::new();
    password_box.set_password_char('•');
    password_box.set_width(260.0);
    if let Some(password) = &password {
        password_box.set_text(Some(password));
    }

    let tls = CheckBox::new();
    tls.set_content(Some(Rc::new("TLS".to_string())));
    tls.set_is_checked(Some(settings.tls));
    let remember = CheckBox::new();
    remember.set_content(Some(Rc::new("Remember password".to_string())));
    remember.set_is_checked(Some(settings.remember_password));

    let connect_button = Button::new();
    connect_button.set_content(Some(Rc::new("Connect".to_string())));
    let forget = icons::icon_button(icons::Icon::Shield, "Forget pinned key");
    let connect_status = text("not connected", 14.0);
    let error_text = text("", 13.0);

    let update_status = text("", 12.0);
    let update_check = icons::icon_button(icons::Icon::Restart, "Check for updates");
    let update_open = icons::icon_button(icons::Icon::OpenExternal, "Open release");
    update_open.set_is_visible(false);
    let update_auto = CheckBox::new();
    update_auto.set_content(Some(Rc::new("Check at startup".to_string())));
    update_auto.set_is_checked(Some(settings.check_for_updates));
    let update_row = row();
    update_row
        .children()
        .add(text(&format!("rust-chat {APP_VERSION}"), 12.0));
    update_row.children().add(update_check.clone());
    update_row.children().add(update_open.clone());
    update_row.children().add(update_auto.clone());
    update_row.children().add(update_status.clone());
    let (update_tx, update_rx) = std::sync::mpsc::channel();
    if settings.check_for_updates {
        let tx = update_tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(update::check(APP_VERSION));
        });
    }

    let trust_text = text("", 13.0);
    let approve = Button::new();
    approve.set_content(Some(Rc::new("Approve".to_string())));
    let deny = Button::new();
    deny.set_content(Some(Rc::new("Deny".to_string())));
    let trust_actions = row();
    trust_actions.children().add(approve.clone());
    trust_actions.children().add(deny.clone());
    let trust_panel = StackPanel::new();
    trust_panel.set_spacing(6.0);
    trust_panel.children().add(trust_text.clone());
    trust_panel.children().add(trust_actions);
    trust_panel.set_is_visible(false);

    let connect_panel = StackPanel::new();
    connect_panel.set_spacing(10.0);
    connect_panel.children().add(text("rust-chat", 26.0));
    connect_panel
        .children()
        .add(text("Connect to a self-hosted server", 14.0));
    connect_panel.children().add(row_with("Host", host.clone()));
    connect_panel.children().add(row_with("Port", port.clone()));
    connect_panel
        .children()
        .add(row_with("Display name", name.clone()));
    connect_panel
        .children()
        .add(row_with("Password", password_box.clone()));
    let options = row();
    options.children().add(tls.clone());
    options.children().add(remember.clone());
    connect_panel.children().add(options);
    let actions = row();
    actions.children().add(connect_button.clone());
    actions.children().add(forget.clone());
    actions.children().add(settings_screen.open_connect.clone());
    connect_panel.children().add(actions);
    connect_panel.children().add(connect_status.clone());
    connect_panel.children().add(error_text.clone());
    connect_panel.children().add(trust_panel.clone());

    let room_title = text("", 20.0);
    let room_status = text("", 13.0);
    let members_panel = StackPanel::new();
    members_panel.set_spacing(2.0);
    let rooms_panel = StackPanel::new();
    rooms_panel.set_spacing(4.0);
    let join_voice = icons::icon_button(icons::Icon::Microphone, "Join voice");
    let leave_voice = icons::icon_button(icons::Icon::Disconnect, "Leave voice");
    let mute_mic = icons::icon_button(icons::Icon::MicrophoneOff, "Mute microphone");
    let mute_speaker = icons::icon_button(icons::Icon::Speaker, "Mute speakers");
    let share = icons::icon_button(icons::Icon::ShareScreen, "Share screen");
    let share_sources = enumerate_sources();
    let share_source_index = settings
        .share_source
        .as_deref()
        .and_then(|id| share_sources.iter().position(|info| info.source.id() == id))
        .unwrap_or(0);
    let share_preset_index = SHARE_PRESETS
        .iter()
        .position(|preset| {
            preset.0 == settings.share_width
                && preset.1 == settings.share_height
                && preset.2 == settings.share_fps
        })
        .unwrap_or(1);
    let share_source = Button::new();
    share_source.set_content(Some(Rc::new(format!(
        "Source: {}",
        share_sources
            .get(share_source_index)
            .map(|info| info.label.as_str())
            .unwrap_or("Monitor 1")
    ))));
    let share_quality = Button::new();
    share_quality.set_content(Some(Rc::new(format!(
        "Quality: {}",
        preset_label(SHARE_PRESETS[share_preset_index])
    ))));
    let share_row = row();
    share_row.children().add(share_source.clone());
    share_row.children().add(share_quality.clone());
    let request_control = icons::icon_button(icons::Icon::RemoteControl, "Request remote control");
    request_control.set_is_visible(false);
    let stop_control = icons::icon_button(icons::Icon::Close, "Stop remote control");
    stop_control.set_is_visible(false);
    let allow_control = icons::icon_button(icons::Icon::Check, "Allow remote control");
    allow_control.set_is_visible(false);
    let deny_control = icons::icon_button(icons::Icon::Close, "Deny remote control");
    deny_control.set_is_visible(false);
    let control_status = text("", 13.0);
    let control_row = row();
    control_row.children().add(request_control.clone());
    control_row.children().add(stop_control.clone());
    control_row.children().add(allow_control.clone());
    control_row.children().add(deny_control.clone());
    control_row.children().add(control_status.clone());
    let voice_status = text("voice: off", 13.0);

    let surface = Rc::new(RefCell::new(
        VideoSurface::new().expect("the Direct2D video surface must be available"),
    ));
    let (presenter, presenter_sub) = build_presenter(surface.borrow().mailbox());
    presenter.set_width(640.0);
    presenter.set_height(360.0);
    presenter.set_horizontal_alignment(HorizontalAlignment::Center);
    presenter.set_is_visible(false);
    let video_host = Grid::new();
    video_host.set_width(640.0);
    video_host.set_height(360.0);
    video_host.set_horizontal_alignment(HorizontalAlignment::Center);
    video_host.set_background(Some(Brushes::transparent()));
    video_host.set_is_visible(false);
    video_host.children().add(presenter.clone());
    let video_label = text("SCREEN SHARE", 12.0);
    video_label.set_is_visible(false);

    let feed_panel = StackPanel::new();
    feed_panel.set_spacing(10.0);
    let feed_scroll = ScrollViewer::new();
    feed_scroll.set_vertical_scroll_bar_visibility(ScrollBarVisibility::Auto);
    feed_scroll.set_horizontal_scroll_bar_visibility(ScrollBarVisibility::Disabled);
    feed_scroll.set_content(Some(Control::boxed(feed_panel.clone())));
    let composer = TextBox::new();
    composer.set_accepts_return(false);
    composer.set_horizontal_alignment(HorizontalAlignment::Stretch);
    let send = icons::icon_button(icons::Icon::Send, "Send message");
    let composer_row = Grid::new();
    composer_row.set_column_definitions(ColumnDefinitions::from_items([
        ColumnDefinition::with_width(GridLength::new(1.0, GridUnitType::Star)),
        ColumnDefinition::with_width(GridLength::AUTO),
    ]));
    Grid::set_column(&composer, 0);
    Grid::set_column(&send, 1);
    composer_row.children().add(&composer);
    composer_row.children().add(&send);

    let file_members = StackPanel::new();
    file_members.set_orientation(Orientation::Horizontal);
    file_members.set_spacing(6.0);
    let file_path = TextBox::new();
    file_path.set_width(420.0);
    let file_caption = TextBox::new();
    file_caption.set_width(160.0);
    let file_choose = icons::icon_button(icons::Icon::Attach, "Choose a file");
    let file_send = icons::icon_button(icons::Icon::Upload, "Send file");
    let file_row = row();
    file_row.children().add(file_choose.clone());
    file_row.children().add(file_path.clone());
    file_row.children().add(file_caption.clone());
    file_row.children().add(file_send.clone());
    let file_offer_text = text("", 13.0);
    let file_accept = icons::icon_button(icons::Icon::Check, "Accept file");
    let file_decline = icons::icon_button(icons::Icon::Close, "Decline file");
    let file_offer_actions = row();
    file_offer_actions.children().add(file_accept.clone());
    file_offer_actions.children().add(file_decline.clone());
    let file_offer_panel = StackPanel::new();
    file_offer_panel.set_spacing(4.0);
    file_offer_panel.children().add(file_offer_text.clone());
    file_offer_panel.children().add(file_offer_actions);
    file_offer_panel.set_is_visible(false);
    let file_status = text("", 13.0);
    let file_cancel = icons::icon_button(icons::Icon::Close, "Cancel transfer");
    file_cancel.set_is_visible(false);
    let file_open = icons::icon_button(icons::Icon::Download, "Open received file");
    file_open.set_is_visible(false);
    let file_actions = row();
    file_actions.children().add(file_cancel.clone());
    file_actions.children().add(file_open.clone());
    file_actions.children().add(file_status.clone());
    let file_panel = StackPanel::new();
    file_panel.set_spacing(6.0);
    file_panel.children().add(text("FILES", 12.0));
    file_panel.children().add(file_members.clone());
    file_panel.children().add(file_row);
    file_panel.children().add(file_offer_panel.clone());
    file_panel.children().add(file_actions);
    file_panel.set_is_visible(false);

    let disconnect = icons::icon_button(icons::Icon::Disconnect, "Disconnect");
    let open_settings_room = settings_screen.open_room.clone();

    // ---- sidebar: brand, rooms, members, navigation ----
    let room_mark = Border::new();
    room_mark.set_width(30.0);
    room_mark.set_height(30.0);
    room_mark.set_background(Some(ui_kit::brush(ui_kit::ACCENT)));
    room_mark.set_corner_radius(CornerRadius::uniform(8.0));
    let mark_glyph = icons::icon(icons::Icon::Chat, 16.0);
    mark_glyph.set_foreground(Some(ui_kit::brush(0xFFFF_FFFF)));
    room_mark.set_child(&mark_glyph);

    let brand_text = StackPanel::new();
    brand_text.set_spacing(1.0);
    brand_text.children().add(ui_kit::body("rust-chat", 14.0));
    let server_line = ui_kit::muted("", 11.0);
    brand_text.children().add(&server_line);
    let brand_row = ui_kit::row(9.0);
    brand_row.children().add(&room_mark);
    brand_row.children().add(&brand_text);

    let settings_nav = icons::icon_label(icons::Icon::Settings, "Settings");
    let settings_nav_button = Button::new();
    settings_nav_button.set_content(Some(Rc::new(settings_nav) as ferroui_base::BoxedValue));
    let disconnect_nav = icons::icon_label(icons::Icon::Disconnect, "Disconnect");
    let disconnect_nav_button = Button::new();
    disconnect_nav_button.set_content(Some(Rc::new(disconnect_nav) as ferroui_base::BoxedValue));

    let sidebar = StackPanel::new();
    sidebar.set_spacing(6.0);
    sidebar.children().add(&brand_row);
    sidebar.children().add(ui_kit::section_label("ROOMS"));
    sidebar.children().add(&rooms_panel);
    sidebar
        .children()
        .add(ui_kit::section_label("IN THIS ROOM"));
    sidebar.children().add(&members_panel);
    sidebar.children().add(ui_kit::divider());
    sidebar.children().add(&settings_nav_button);
    sidebar.children().add(&disconnect_nav_button);
    let sidebar_frame = Border::new();
    sidebar_frame.set_background(Some(ui_kit::brush(ui_kit::RAIL)));
    sidebar_frame.set_padding(Thickness {
        left: 12.0,
        top: 12.0,
        right: 12.0,
        bottom: 12.0,
    });
    sidebar_frame.set_border_brush(Some(ui_kit::brush(ui_kit::STROKE)));
    sidebar_frame.set_border_thickness(Thickness {
        left: 0.0,
        top: 0.0,
        right: 1.0,
        bottom: 0.0,
    });
    sidebar_frame.set_child(&sidebar);

    // ---- centre: header, stage, feed, composer ----
    let header_text = StackPanel::new();
    header_text.set_spacing(2.0);
    header_text.children().add(&room_title);
    header_text.children().add(&room_status);
    let header_actions = ui_kit::row(6.0);
    header_actions.children().add(&join_voice);
    header_actions.children().add(&leave_voice);
    header_actions.children().add(&share);
    header_actions.children().add(&open_settings_room);
    header_actions.children().add(&disconnect);
    let header_grid = Grid::new();
    header_grid.set_column_definitions(ColumnDefinitions::from_items([
        ColumnDefinition::with_width(GridLength::new(1.0, GridUnitType::Star)),
        ColumnDefinition::with_width(GridLength::AUTO),
    ]));
    Grid::set_column(&header_text, 0);
    Grid::set_column(&header_actions, 1);
    header_grid.children().add(&header_text);
    header_grid.children().add(&header_actions);
    let header_frame = Border::new();
    header_frame.set_background(Some(ui_kit::brush(ui_kit::CARD)));
    header_frame.set_padding(Thickness {
        left: 14.0,
        top: 9.0,
        right: 14.0,
        bottom: 9.0,
    });
    header_frame.set_border_brush(Some(ui_kit::brush(ui_kit::STROKE)));
    header_frame.set_border_thickness(Thickness {
        left: 0.0,
        top: 0.0,
        right: 0.0,
        bottom: 1.0,
    });
    header_frame.set_child(&header_grid);

    video_label.set_foreground(Some(ui_kit::brush(0xFFCF_D6E0)));
    let stage_inner = StackPanel::new();
    stage_inner.set_spacing(6.0);
    stage_inner.children().add(&video_label);
    stage_inner.children().add(&video_host);
    let stage_frame = Border::new();
    stage_frame.set_background(Some(ui_kit::brush(0xFF0E_1116)));
    stage_frame.set_corner_radius(CornerRadius::uniform(10.0));
    stage_frame.set_padding(Thickness::uniform(8.0));
    stage_frame.set_margin(Thickness {
        left: 14.0,
        top: 10.0,
        right: 14.0,
        bottom: 0.0,
    });
    stage_frame.set_child(&stage_inner);

    let feed_frame = Border::new();
    feed_frame.set_background(Some(ui_kit::brush(ui_kit::CARD)));
    feed_frame.set_border_brush(Some(ui_kit::brush(ui_kit::STROKE)));
    feed_frame.set_border_thickness(Thickness::uniform(1.0));
    feed_frame.set_corner_radius(CornerRadius::uniform(10.0));
    feed_frame.set_padding(Thickness::uniform(8.0));
    feed_frame.set_margin(Thickness {
        left: 14.0,
        top: 10.0,
        right: 14.0,
        bottom: 0.0,
    });
    feed_frame.set_child(&feed_scroll);

    let composer_frame = Border::new();
    composer_frame.set_padding(Thickness {
        left: 14.0,
        top: 8.0,
        right: 14.0,
        bottom: 12.0,
    });
    composer_frame.set_child(&composer_row);

    let centre = Grid::new();
    centre.set_row_definitions(RowDefinitions::from_items([
        RowDefinition::with_height(GridLength::AUTO),
        RowDefinition::with_height(GridLength::AUTO),
        RowDefinition::with_height(GridLength::new(1.0, GridUnitType::Star)),
        RowDefinition::with_height(GridLength::AUTO),
    ]));
    Grid::set_row(&header_frame, 0);
    Grid::set_row(&stage_frame, 1);
    Grid::set_row(&feed_frame, 2);
    Grid::set_row(&composer_frame, 3);
    centre.children().add(&header_frame);
    centre.children().add(&stage_frame);
    centre.children().add(&feed_frame);
    centre.children().add(&composer_frame);

    // ---- right rail: voice, files, sharing, about ----
    let voice_panel = StackPanel::new();
    voice_panel.set_spacing(8.0);
    voice_panel.children().add(ui_kit::section_label("VOICE"));
    voice_panel.children().add(&voice_status);
    let voice_buttons = ui_kit::row(6.0);
    voice_buttons.children().add(&mute_mic);
    voice_buttons.children().add(&mute_speaker);
    voice_panel.children().add(&voice_buttons);

    let share_panel = StackPanel::new();
    share_panel.set_spacing(8.0);
    share_panel.children().add(ui_kit::section_label("SHARING"));
    share_panel.children().add(&share_row);
    share_panel.children().add(&control_row);

    let about_panel = StackPanel::new();
    about_panel.set_spacing(8.0);
    about_panel.children().add(ui_kit::section_label("ABOUT"));
    about_panel.children().add(&update_row);

    let rail = StackPanel::new();
    rail.set_spacing(10.0);
    rail.children().add(ui_kit::card(&voice_panel, 12.0));
    let file_card = ui_kit::card(&file_panel, 12.0);
    rail.children().add(&file_card);
    let share_card = ui_kit::card(&share_panel, 12.0);
    rail.children().add(&share_card);
    rail.children().add(ui_kit::card(&about_panel, 12.0));
    let rail_frame = Border::new();
    rail_frame.set_background(Some(ui_kit::brush(ui_kit::RAIL)));
    rail_frame.set_padding(Thickness {
        left: 12.0,
        top: 12.0,
        right: 12.0,
        bottom: 12.0,
    });
    rail_frame.set_border_brush(Some(ui_kit::brush(ui_kit::STROKE)));
    rail_frame.set_border_thickness(Thickness {
        left: 1.0,
        top: 0.0,
        right: 0.0,
        bottom: 0.0,
    });
    rail_frame.set_child(&rail);

    let room_view = Grid::new();
    room_view.set_column_definitions(ColumnDefinitions::from_items([
        ColumnDefinition::with_width(GridLength::new(236.0, GridUnitType::Pixel)),
        ColumnDefinition::with_width(GridLength::new(1.0, GridUnitType::Star)),
        ColumnDefinition::with_width(GridLength::new(284.0, GridUnitType::Pixel)),
    ]));
    Grid::set_column(&sidebar_frame, 0);
    Grid::set_column(&centre, 1);
    Grid::set_column(&rail_frame, 2);
    room_view.children().add(&sidebar_frame);
    room_view.children().add(&centre);
    room_view.children().add(&rail_frame);
    room_view.set_is_visible(false);

    let root = Grid::new();
    root.set_horizontal_alignment(HorizontalAlignment::Stretch);
    root.set_vertical_alignment(VerticalAlignment::Stretch);
    root.set_background(Some(ui_kit::brush(ui_kit::BG)));
    root.children().add(connect_panel.clone());
    root.children().add(room_view.clone());
    root.children().add(settings_screen.view.clone());

    let file_prefs = FilePrefs::new(
        settings.downloads_dir.as_ref().map(PathBuf::from),
        settings.open_when_finished,
    );

    let ui = Rc::new(Ui {
        client: startup.client,
        memory: startup.memory,
        runtime: startup.runtime,
        settings: RefCell::new(settings.clone()),
        connect_panel,
        room_view: room_view.clone(),
        stage_frame: stage_frame.clone(),
        file_card: file_card.clone(),
        share_card: share_card.clone(),
        server_line: server_line.clone(),
        settings_nav: settings_nav_button.clone(),
        disconnect_nav: disconnect_nav_button.clone(),
        host,
        port,
        name,
        password: password_box,
        tls,
        remember,
        connect_status,
        error_text,
        trust_panel,
        trust_text,
        room_title,
        room_status,
        members_panel,
        members_sig: RefCell::new(String::new()),
        rooms_panel,
        feed_panel,
        feed_scroll,
        composer,
        last_rooms: RefCell::new(String::new()),
        last_messages: RefCell::new(String::new()),
        connect_button,
        approve,
        deny,
        send,
        forget,
        disconnect,
        join_voice,
        leave_voice,
        mute_mic,
        mute_speaker,
        share,
        share_source,
        share_quality,
        share_sources: RefCell::new(share_sources),
        share_source_index: Cell::new(share_source_index),
        share_preset_index: Cell::new(share_preset_index),
        request_control,
        stop_control,
        allow_control,
        deny_control,
        control_status,
        control_pending: Cell::new(false),
        control_peer: RefCell::new(None),
        control_until: Cell::new(None),
        control_host_peer: RefCell::new(None),
        control_host_until: Cell::new(None),
        control_request: RefCell::new(None),
        last_mouse: Cell::new(Instant::now()),
        input_tokens: RefCell::new(Vec::new()),
        control_line: RefCell::new(String::new()),
        file_members,
        file_path,
        file_caption,
        file_send,
        file_choose,
        file_offer_panel,
        file_offer_text,
        file_accept,
        file_decline,
        file_status,
        file_cancel,
        file_open,
        file_panel,
        file_target: RefCell::new(None),
        file_offers: RefCell::new(Vec::new()),
        file_names: RefCell::new(HashMap::new()),
        file_active: RefCell::new(None),
        file_last: RefCell::new(None),
        file_members_sig: RefCell::new(String::new()),
        file_line: RefCell::new(String::new()),
        file_shown: RefCell::new(String::new()),
        update_status,
        update_check,
        update_open,
        update_auto,
        update_rx: RefCell::new(Some(update_rx)),
        update_tx,
        update_url: RefCell::new(None),
        presenter,
        video_host,
        surface,
        _presenter_sub: RefCell::new(Some(presenter_sub)),
        last_video: RefCell::new(None),
        video_visible: Cell::new(false),
        video_label,
        voice_status,
        voice_note: RefCell::new(None),
        voice_line: RefCell::new(String::new()),
        voice_wanted: Cell::new(false),
        voice_room_id: RefCell::new(None),
        voice: RefCell::new(None),
        settings_screen,
        file_prefs,
    });

    wire(&ui);
    attach_control_input(&ui);
    let timer =
        DispatcherTimer::with_callback(Duration::from_millis(100), DispatcherPriority::NORMAL, {
            let ui = ui.clone();
            move |_| refresh(&ui)
        });
    TIMER.with(|cell| *cell.borrow_mut() = Some(timer));
    refresh(&ui);

    if settings.auto_connect && password.as_deref().is_some_and(|value| !value.is_empty()) {
        ui.connect();
    }
    if let Some(request) = smoke_request(&settings) {
        ui.host.set_text(Some(&request.host));
        ui.port.set_text(Some(&request.port.to_string()));
        ui.name.set_text(Some(&request.name));
        ui.password.set_text(Some(&request.password));
        ui.tls.set_is_checked(Some(request.tls));
        let _ = ui.client.connect(request);
    }
    if std::env::var_os("RUST_CHAT_OPEN_SETTINGS").is_some() {
        ui.set_settings_visible(true);
    }

    let window = Window::new();
    window.set_title(Some(format!("rust-chat {APP_VERSION}")));
    let area = work_area();
    let scale = system_scale();
    let height = area
        .map(|area| (f64::from(area.height) / scale - 120.0).clamp(600.0, 920.0))
        .unwrap_or(920.0);
    let width = area
        .map(|area| (f64::from(area.width) / scale - 60.0).clamp(760.0, 1100.0))
        .unwrap_or(1100.0);
    window.set_width(width);
    window.set_height(height);
    if let Some(area) = area {
        // Center in the work area so a short screen never pushes the window under the taskbar.
        let outer_width = ((width + 16.0) * scale).round() as i32;
        let outer_height = ((height + 40.0) * scale).round() as i32;
        let x = area.left + (area.width as i32 - outer_width).max(0) / 2;
        let y = area.top + (area.height as i32 - outer_height).max(0) / 2;
        window.set_position(ferroui_base::PixelPoint { x, y });
    }
    window.set_padding(Thickness::uniform(18.0));
    window.set_content(Some(Control::boxed(root)));
    window
        .closed({
            let client = ui.client.clone();
            let weak = Rc::downgrade(&ui);
            move || {
                if let Some(ui) = weak.upgrade() {
                    // Keep the endpoint and display name even if the user never connected.
                    ui.save_connection_form();
                    ui.leave_voice(Some("voice: window closed"));
                }
                let _ = client.disconnect();
            }
        })
        .detach();
    window
}

fn wire(ui: &Rc<Ui>) {
    ui.connect_button.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.connect();
            }
        }
    });
    ui.forget.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.forget_key();
            }
        }
    });
    ui.remember.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                // Remembering a password must survive a restart on its own.
                ui.save_connection_form();
            }
        }
    });
    ui.approve.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                let _ = ui.client.trust(true);
            }
        }
    });
    ui.deny.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                let _ = ui.client.trust(false);
            }
        }
    });
    ui.send.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.send_message();
            }
        }
    });
    ui.disconnect.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.leave_voice(Some("voice: disconnected"));
                let _ = ui.client.disconnect();
            }
        }
    });
    ui.join_voice.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.join_voice();
            }
        }
    });
    ui.leave_voice.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.leave_voice(None);
            }
        }
    });
    ui.mute_mic.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.toggle_mic();
            }
        }
    });
    ui.mute_speaker.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.toggle_speaker();
            }
        }
    });
    ui.share.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.toggle_share();
            }
        }
    });
    ui.share_source.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.cycle_share_source();
            }
        }
    });
    ui.share_quality.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.cycle_share_quality();
            }
        }
    });
    ui.request_control.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.request_control();
            }
        }
    });
    ui.stop_control.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.stop_control();
            }
        }
    });
    ui.allow_control.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.respond_control(true);
            }
        }
    });
    ui.deny_control.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.respond_control(false);
            }
        }
    });
    ui.file_choose.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.choose_file();
            }
        }
    });
    ui.file_send.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.send_file();
            }
        }
    });
    ui.file_accept.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.accept_offer();
            }
        }
    });
    ui.file_decline.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.decline_offer();
            }
        }
    });
    ui.file_cancel.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.cancel_file();
            }
        }
    });
    ui.file_open.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.open_last_file();
            }
        }
    });
    ui.update_check.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.check_updates();
            }
        }
    });
    ui.update_open.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.open_release();
            }
        }
    });
    ui.update_auto.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.set_update_auto(ui.update_auto.is_checked().unwrap_or(true));
            }
        }
    });

    let screen = &ui.settings_screen;
    for button in [screen.open_room.clone(), screen.open_connect.clone()] {
        button.click({
            let weak = Rc::downgrade(ui);
            move |_, _| {
                if let Some(ui) = weak.upgrade() {
                    ui.set_settings_visible(true);
                }
            }
        });
    }
    ui.settings_nav.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.set_settings_visible(true);
            }
        }
    });
    for (index, button) in screen.nav.iter().enumerate() {
        button.click({
            let weak = Rc::downgrade(ui);
            move |_, _| {
                if let Some(ui) = weak.upgrade() {
                    ui.select_settings_section(index);
                }
            }
        });
    }
    ui.disconnect_nav.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.leave_voice(Some("voice: disconnected"));
                let _ = ui.client.disconnect();
            }
        }
    });
    screen.close.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.cancel_confirm();
                ui.set_settings_visible(false);
            }
        }
    });
    screen.auto_connect.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                let checked = ui
                    .settings_screen
                    .auto_connect
                    .is_checked()
                    .unwrap_or(false);
                ui.change_settings(|settings| settings.auto_connect = checked);
            }
        }
    });
    screen.timestamps.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                let checked = ui.settings_screen.timestamps.is_checked().unwrap_or(true);
                ui.change_settings(|settings| settings.show_timestamps = checked);
            }
        }
    });
    screen.clock24.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                let checked = ui.settings_screen.clock24.is_checked().unwrap_or(true);
                ui.change_settings(|settings| settings.clock_24h = checked);
            }
        }
    });
    screen.keep_history.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                let checked = ui.settings_screen.keep_history.is_checked().unwrap_or(true);
                ui.change_settings(|settings| settings.keep_history = checked);
            }
        }
    });
    screen.open_when_finished.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                let checked = ui
                    .settings_screen
                    .open_when_finished
                    .is_checked()
                    .unwrap_or(false);
                ui.change_settings(|settings| settings.open_when_finished = checked);
            }
        }
    });
    screen.mic.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.cycle_device(true);
            }
        }
    });
    screen.speaker.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.cycle_device(false);
            }
        }
    });
    screen.quality.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.cycle_settings_quality();
            }
        }
    });
    screen.choose_downloads.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.choose_downloads();
            }
        }
    });
    screen.reset_downloads.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.reset_downloads();
            }
        }
    });
    screen.forget_pin.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.arm_confirm(ConfirmAction::ForgetPin);
            }
        }
    });
    screen.clear_history.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.arm_confirm(ConfirmAction::ClearHistory);
            }
        }
    });
    screen.confirm.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.run_confirm();
            }
        }
    });
    screen.cancel.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.cancel_confirm();
            }
        }
    });
}

fn attach_control_input(ui: &Rc<Ui>) {
    ui.presenter.set_focusable(true);
    ui.presenter.set_is_tab_stop(true);
    let mut tokens = Vec::new();
    tokens.push(
        ui.video_host
            .add_handler(InputElement::pointer_moved_event(), {
                let weak = Rc::downgrade(ui);
                move |_, e| {
                    if let Some(ui) = weak.upgrade() {
                        ui.on_pointer_moved(e);
                    }
                }
            }),
    );
    tokens.push(
        ui.video_host
            .add_handler(InputElement::pointer_pressed_event(), {
                let weak = Rc::downgrade(ui);
                move |_, e| {
                    if let Some(ui) = weak.upgrade() {
                        ui.on_pointer_pressed(e);
                    }
                }
            }),
    );
    tokens.push(
        ui.video_host
            .add_handler(InputElement::pointer_released_event(), {
                let weak = Rc::downgrade(ui);
                move |_, e| {
                    if let Some(ui) = weak.upgrade() {
                        ui.on_pointer_released(e);
                    }
                }
            }),
    );
    tokens.push(
        ui.video_host
            .add_handler(InputElement::pointer_wheel_changed_event(), {
                let weak = Rc::downgrade(ui);
                move |_, e| {
                    if let Some(ui) = weak.upgrade() {
                        ui.on_pointer_wheel(e);
                    }
                }
            }),
    );
    tokens.push(ui.presenter.add_handler(InputElement::key_down_event(), {
        let weak = Rc::downgrade(ui);
        move |_, e| {
            if let Some(ui) = weak.upgrade() {
                ui.on_key(e, true);
            }
        }
    }));
    tokens.push(ui.presenter.add_handler(InputElement::key_up_event(), {
        let weak = Rc::downgrade(ui);
        move |_, e| {
            if let Some(ui) = weak.upgrade() {
                ui.on_key(e, false);
            }
        }
    }));
    ui.input_tokens.borrow_mut().extend(tokens);
}

fn refresh(ui: &Rc<Ui>) {
    let snapshot = ui.client.snapshot();
    let room_visible = snapshot.room.is_some();
    let settings_open = ui.settings_screen.visible.get();
    ui.room_view.set_is_visible(room_visible && !settings_open);
    ui.connect_panel
        .set_is_visible(!room_visible && !settings_open);
    ui.settings_screen.view.set_is_visible(settings_open);

    let mut status = snapshot.connection.label();
    if let ConnectionState::Failed(failure) = &snapshot.connection
        && let Some(hint) = &failure.hint
    {
        status.push('\n');
        status.push_str(hint);
    }
    ui.connect_status.set_text(Some(&status));
    ui.error_text
        .set_text(Some(snapshot.last_error.as_deref().unwrap_or("")));

    let update_result = ui
        .update_rx
        .borrow()
        .as_ref()
        .and_then(|rx| rx.try_recv().ok());
    if let Some(result) = update_result {
        match result {
            Ok(Some(release)) => {
                ui.update_status
                    .set_text(Some(&format!("version {} is available", release.version)));
                *ui.update_url.borrow_mut() = Some(release.url);
                ui.update_open.set_is_visible(true);
            }
            Ok(None) => ui
                .update_status
                .set_text(Some(&format!("up to date (v{APP_VERSION})"))),
            Err(error) => ui.update_status.set_text(Some(&error)),
        }
    }

    if let Some(trust) = &snapshot.trust {
        ui.trust_panel.set_is_visible(true);
        let text = format!(
            "Server key\n{}\nApprove only if it matches the fingerprint the server printed.",
            trust.fingerprint
        );
        ui.trust_text.set_text(Some(&text));
    } else {
        ui.trust_panel.set_is_visible(false);
    }

    if snapshot.room.is_none() {
        ui.leave_voice(Some("off"));
        ui.join_voice.set_is_visible(false);
        ui.leave_voice.set_is_visible(false);
        ui.mute_mic.set_is_visible(false);
        ui.mute_speaker.set_is_visible(false);
        ui.voice_status.set_text(Some(""));
    }

    if let Some(room) = &snapshot.room {
        refresh_voice(ui, &snapshot);
        let presenting = room
            .members
            .iter()
            .any(|member| member.sharing && member.session_id != room.self_member.session_id);
        render_video(ui, presenting);
        ui.stage_frame.set_is_visible(presenting);
        let title = format!(
            "# {}   ·   {} member{}",
            room.room.name,
            room.members.len(),
            if room.members.len() == 1 { "" } else { "s" }
        );
        ui.room_title.set_text(Some(&title));
        let connection = match snapshot.latency_ms {
            Some(latency) if snapshot.connection.is_ready() => {
                format!("{}   ·   {latency} ms", snapshot.connection.label())
            }
            _ => snapshot.connection.label(),
        };
        ui.room_status.set_text(Some(&connection));
        let endpoint = {
            let settings = ui.settings.borrow();
            match snapshot.latency_ms {
                Some(latency) if snapshot.connection.is_ready() => {
                    format!("{}:{} · {latency} ms", settings.host, settings.port)
                }
                _ => format!("{}:{}", settings.host, settings.port),
            }
        };
        ui.server_line.set_text(Some(&endpoint));

        let levels = ui
            .voice
            .borrow()
            .as_ref()
            .map(|runtime| runtime.levels.borrow().clone())
            .unwrap_or_default();
        let stats = ui
            .voice
            .borrow()
            .as_ref()
            .map(|runtime| runtime.stats.borrow().clone())
            .unwrap_or_default();
        let mut member_rows: Vec<Ref<StackPanel>> = Vec::new();
        for member in &room.members {
            let is_self = member.session_id == room.self_member.session_id;
            let initial = member
                .name
                .chars()
                .next()
                .map(|ch| ch.to_uppercase().to_string())
                .unwrap_or_else(|| "?".to_owned());
            let avatar = Border::new();
            avatar.set_width(24.0);
            avatar.set_height(24.0);
            avatar.set_background(Some(ui_kit::brush(if is_self {
                0xFFD9_E7F5
            } else {
                0xFFE8_EEF5
            })));
            avatar.set_corner_radius(CornerRadius::uniform(12.0));
            let initial_text = ui_kit::body(&initial, 11.0);
            initial_text.set_horizontal_alignment(HorizontalAlignment::Center);
            initial_text.set_vertical_alignment(VerticalAlignment::Center);
            avatar.set_child(&initial_text);

            let mut name = member.name.clone();
            if is_self {
                name.push_str(" (you)");
            }
            let name_text = ui_kit::body(&name, 12.5);

            let states = ui_kit::row(4.0);
            if member.self_mute {
                states
                    .children()
                    .add(icons::icon(icons::Icon::MicrophoneOff, 13.0));
            }
            if member.sharing {
                states
                    .children()
                    .add(icons::icon(icons::Icon::ShareScreen, 13.0));
            }
            if member.voice {
                states
                    .children()
                    .add(icons::icon(icons::Icon::Microphone, 13.0));
            }
            if let Some(label) = stats.get(&member.session_id)
                && !label.is_empty()
            {
                states.children().add(ui_kit::muted(label, 10.5));
            }
            if speaking(&levels, if is_self { "" } else { &member.session_id }) {
                states.children().add(ui_kit::muted("\u{266A}", 12.0));
            }

            let row_grid = Grid::new();
            row_grid.set_column_definitions(ColumnDefinitions::from_items([
                ColumnDefinition::with_width(GridLength::AUTO),
                ColumnDefinition::with_width(GridLength::new(1.0, GridUnitType::Star)),
                ColumnDefinition::with_width(GridLength::AUTO),
            ]));
            Grid::set_column(&avatar, 0);
            Grid::set_column(&name_text, 1);
            Grid::set_column(&states, 2);
            row_grid.children().add(&avatar);
            row_grid.children().add(&name_text);
            row_grid.children().add(&states);
            let row_panel = StackPanel::new();
            row_panel.children().add(&row_grid);
            member_rows.push(row_panel);
        }
        let members_signature = room
            .members
            .iter()
            .map(|member| {
                format!(
                    "{}:{}:{}:{}:{}",
                    member.session_id, member.name, member.self_mute, member.sharing, member.voice
                )
            })
            .collect::<Vec<_>>()
            .join("|");
        if *ui.members_sig.borrow() != members_signature {
            *ui.members_sig.borrow_mut() = members_signature;
            ui.members_panel.children().clear();
            for row in &member_rows {
                ui.members_panel.children().add(row);
            }
        }

        let rooms_signature = snapshot
            .rooms
            .iter()
            .map(|room| format!("{}:{}:{}", room.id, room.member_count, room.name))
            .collect::<Vec<_>>()
            .join("|");
        if *ui.last_rooms.borrow() != rooms_signature {
            *ui.last_rooms.borrow_mut() = rooms_signature;
            ui.rooms_panel.children().clear();
            for summary in &snapshot.rooms {
                let active = summary.id == room.room.id;
                let hash = ui_kit::muted("#", 13.0);
                let name = ui_kit::body(&summary.name, 13.0);
                let count = ui_kit::muted(&summary.member_count.to_string(), 11.5);
                let row_grid = Grid::new();
                row_grid.set_column_definitions(ColumnDefinitions::from_items([
                    ColumnDefinition::with_width(GridLength::AUTO),
                    ColumnDefinition::with_width(GridLength::new(1.0, GridUnitType::Star)),
                    ColumnDefinition::with_width(GridLength::AUTO),
                ]));
                Grid::set_column(&hash, 0);
                Grid::set_column(&name, 1);
                Grid::set_column(&count, 2);
                row_grid.children().add(&hash);
                row_grid.children().add(&name);
                row_grid.children().add(&count);

                let button = Button::new();
                button.set_content(Some(Rc::new(row_grid) as ferroui_base::BoxedValue));
                button.set_horizontal_content_alignment(HorizontalAlignment::Stretch);
                button.set_horizontal_alignment(HorizontalAlignment::Stretch);
                if active {
                    button.set_background(Some(ui_kit::brush(ui_kit::SOFT)));
                }
                let weak = Rc::downgrade(ui);
                let room_id = summary.id.clone();
                button.click(move |_, _| {
                    if let Some(ui) = weak.upgrade() {
                        let _ = ui.client.join(room_id.clone());
                    }
                });
                ui.rooms_panel.children().add(button);
            }
        }

        let last_id = snapshot
            .messages
            .last()
            .map(|message| message.message_id.as_str())
            .unwrap_or("");
        let appearance = {
            let settings = ui.settings.borrow();
            (settings.show_timestamps, settings.clock_24h)
        };
        let messages_signature = format!(
            "{}:{}:{}:{}:{}",
            room.room.id,
            snapshot.messages.len(),
            last_id,
            appearance.0,
            appearance.1
        );
        if *ui.last_messages.borrow() != messages_signature {
            *ui.last_messages.borrow_mut() = messages_signature;
            ui.feed_panel.children().clear();
            if snapshot.messages.is_empty() {
                ui.feed_panel
                    .children()
                    .add(ui_kit::muted("No messages yet.", 12.0));
            }
            let self_id = room.self_member.session_id.clone();
            let shown = snapshot.messages.len().saturating_sub(200);
            for message in snapshot.messages.iter().skip(shown) {
                let own = message.from == self_id;
                let header = if appearance.0 {
                    format!(
                        "{} · {}",
                        message.name,
                        local_clock(message.ts, appearance.1)
                    )
                } else {
                    message.name.clone()
                };
                let who = ui_kit::muted(&header, 11.0);
                let text = ui_kit::body(&message.text, 13.5);
                text.set_text_wrapping(ferroui_base::media::TextWrapping::Wrap);
                text.set_max_width(520.0);
                let bubble_inner = StackPanel::new();
                bubble_inner.set_spacing(3.0);
                bubble_inner.children().add(&who);
                bubble_inner.children().add(&text);
                let bubble = Border::new();
                bubble.set_background(Some(ui_kit::brush(if own {
                    0xFFEE_F4FB
                } else {
                    ui_kit::CARD
                })));
                bubble.set_border_brush(Some(ui_kit::brush(if own {
                    0xFFCF_E2F7
                } else {
                    ui_kit::STROKE
                })));
                bubble.set_border_thickness(Thickness::uniform(1.0));
                bubble.set_corner_radius(CornerRadius::uniform(10.0));
                bubble.set_padding(Thickness {
                    left: 12.0,
                    top: 7.0,
                    right: 12.0,
                    bottom: 7.0,
                });
                bubble.set_child(&bubble_inner);
                bubble.set_horizontal_alignment(if own {
                    HorizontalAlignment::Right
                } else {
                    HorizontalAlignment::Left
                });
                let line = StackPanel::new();
                line.set_horizontal_alignment(HorizontalAlignment::Stretch);
                line.children().add(&bubble);
                ui.feed_panel.children().add(&line);
            }
            ui.feed_scroll.scroll_to_end();
        }
    } else {
        ui.last_rooms.borrow_mut().clear();
        ui.last_messages.borrow_mut().clear();
    }
}

fn refresh_voice(ui: &Rc<Ui>, snapshot: &ChatSnapshot) {
    let active_room = snapshot.room.as_ref().map(|room| room.room.id.clone());
    let mut stop = None;
    if let Some(runtime) = ui.voice.borrow().as_ref() {
        if active_room.as_deref() != Some(runtime.room.as_str()) {
            stop = Some("left with the room");
        } else if !snapshot.connection.is_ready() {
            stop = Some("disconnected");
        }
    }
    if let Some(reason) = stop {
        let reconnect = reason == "disconnected";
        ui.leave_voice(Some(reason));
        if reconnect {
            // The WebSocket will come back; rejoin the same room's call automatically.
            ui.voice_wanted.set(true);
        } else {
            *ui.voice_room_id.borrow_mut() = None;
        }
    }
    let Some(room) = snapshot.room.as_ref() else {
        return;
    };

    let mut ended = None;
    let mut peer_error = None;
    {
        let mut slot = ui.voice.borrow_mut();
        if let Some(runtime) = slot.as_mut() {
            loop {
                match runtime.controller.try_recv() {
                    Some(VoiceEvent::Ready(_)) => {
                        *ui.voice_note.borrow_mut() = Some("live".to_owned());
                    }
                    Some(VoiceEvent::Level { peer, level }) => {
                        runtime
                            .levels
                            .borrow_mut()
                            .insert(peer, (level, Instant::now()));
                    }
                    Some(VoiceEvent::SelfLevel(level)) => {
                        runtime
                            .levels
                            .borrow_mut()
                            .insert(String::new(), (level, Instant::now()));
                    }
                    Some(VoiceEvent::Stats {
                        peer,
                        rtt_ms,
                        relayed,
                    }) => {
                        let label = match (relayed, rtt_ms) {
                            (true, Some(rtt)) => format!("relay {rtt} ms"),
                            (true, None) => "relay".to_owned(),
                            (false, Some(rtt)) => format!("{rtt} ms"),
                            (false, None) => String::new(),
                        };
                        runtime.stats.borrow_mut().insert(peer, label);
                    }
                    Some(VoiceEvent::ControlRequested { peer, lease_secs }) => {
                        *ui.control_request.borrow_mut() = Some((peer, lease_secs));
                        *ui.voice_note.borrow_mut() =
                            Some("a participant requests remote control".to_owned());
                    }
                    Some(VoiceEvent::ControlGranted { peer, lease_secs }) => {
                        ui.control_pending.set(false);
                        *ui.control_peer.borrow_mut() = Some(peer);
                        ui.control_until
                            .set(Some(Instant::now() + Duration::from_secs(lease_secs)));
                        *ui.voice_note.borrow_mut() = Some("remote control granted".to_owned());
                    }
                    Some(VoiceEvent::ControlHosting { peer, lease_secs }) => {
                        *ui.control_host_peer.borrow_mut() = Some(peer);
                        ui.control_host_until
                            .set(Some(Instant::now() + Duration::from_secs(lease_secs)));
                        *ui.voice_note.borrow_mut() = Some("remote control active".to_owned());
                    }
                    Some(VoiceEvent::ControlEnded { reason }) => {
                        ui.clear_control();
                        *ui.voice_note.borrow_mut() = Some(format!("control ended: {reason}"));
                    }
                    Some(VoiceEvent::FilePreparing { id, name }) => {
                        ui.file_names.borrow_mut().insert(id.clone(), name.clone());
                        *ui.file_active.borrow_mut() = Some(id);
                        *ui.file_line.borrow_mut() = format!("hashing {name}\u{2026}");
                    }
                    Some(VoiceEvent::FileOutgoing {
                        id,
                        peer,
                        name,
                        size,
                    }) => {
                        ui.file_names.borrow_mut().insert(id.clone(), name.clone());
                        *ui.file_active.borrow_mut() = Some(id);
                        *ui.file_line.borrow_mut() = format!(
                            "offering {name} ({}) to {}",
                            human_size(size),
                            ui.member_name(&peer)
                        );
                    }
                    Some(VoiceEvent::FileOffered {
                        id,
                        peer,
                        name,
                        size,
                        sha256: _,
                        caption,
                    }) => {
                        ui.file_names.borrow_mut().insert(id.clone(), name.clone());
                        ui.file_offers
                            .borrow_mut()
                            .push((id, peer.clone(), name.clone(), size));
                        let suffix = if caption.is_empty() {
                            String::new()
                        } else {
                            format!(" \u{2014} {caption}")
                        };
                        *ui.file_line.borrow_mut() = format!(
                            "{} offers {name} ({}){suffix}",
                            ui.member_name(&peer),
                            human_size(size)
                        );
                        ui.update_offer_panel();
                    }
                    Some(VoiceEvent::FileProgress {
                        id,
                        transferred,
                        total,
                    }) => {
                        *ui.file_active.borrow_mut() = Some(id.clone());
                        let name = ui
                            .file_names
                            .borrow()
                            .get(&id)
                            .cloned()
                            .unwrap_or_else(|| "file".to_owned());
                        let percent = transferred
                            .saturating_mul(100)
                            .checked_div(total)
                            .unwrap_or(0);
                        *ui.file_line.borrow_mut() = format!(
                            "{name}: {percent}% \u{00b7} {}/{}",
                            human_size(transferred),
                            human_size(total)
                        );
                    }
                    Some(VoiceEvent::FileSent { id, path: _ }) => {
                        let name = ui
                            .file_names
                            .borrow()
                            .get(&id)
                            .cloned()
                            .unwrap_or_else(|| "file".to_owned());
                        *ui.file_line.borrow_mut() = format!("sent {name}");
                        *ui.file_active.borrow_mut() = None;
                    }
                    Some(VoiceEvent::FileReceived { id, path }) => {
                        let name = ui
                            .file_names
                            .borrow()
                            .get(&id)
                            .cloned()
                            .unwrap_or_else(|| "file".to_owned());
                        *ui.file_line.borrow_mut() = format!("saved {name} to {path}");
                        *ui.file_active.borrow_mut() = None;
                        *ui.file_last.borrow_mut() = Some(PathBuf::from(&path));
                        if ui.file_prefs.open_when_finished() {
                            open_path(&path);
                        }
                    }
                    Some(VoiceEvent::FileFailed { id, message }) => {
                        *ui.file_line.borrow_mut() = format!("file: {message}");
                        *ui.file_active.borrow_mut() = None;
                        if !id.is_empty() {
                            ui.file_offers.borrow_mut().retain(|offer| offer.0 != id);
                            ui.update_offer_panel();
                        }
                    }
                    Some(VoiceEvent::FileCancelled { id }) => {
                        *ui.file_line.borrow_mut() = "transfer cancelled".to_owned();
                        *ui.file_active.borrow_mut() = None;
                        ui.file_offers.borrow_mut().retain(|offer| offer.0 != id);
                        ui.update_offer_panel();
                    }
                    Some(VoiceEvent::PeerFailed(error)) => peer_error = Some(error),
                    Some(VoiceEvent::Error(error)) => {
                        ended = Some(format!("audio failed: {error}"));
                        break;
                    }
                    Some(VoiceEvent::Ended) => {
                        ended = Some("ended".to_owned());
                        break;
                    }
                    None => break,
                }
            }
            if ended.is_none() {
                let members: Vec<String> = room
                    .members
                    .iter()
                    .filter(|member| member.voice)
                    .map(|member| member.session_id.clone())
                    .collect();
                let signature = members.join(",");
                if *runtime.members_signature.borrow() != signature {
                    *runtime.members_signature.borrow_mut() = signature;
                    runtime.controller.send(VoiceCommand::Members(members));
                }
            }
        }
    }
    if let Some(reason) = ended {
        ui.leave_voice(Some(&reason));
    }
    if let Some(error) = peer_error {
        *ui.voice_note.borrow_mut() = Some(format!("a peer failed: {error}"));
    }

    let active = ui.voice.borrow().is_some();
    let voice_room = room.room.voice;
    if !active
        && voice_room
        && ui.voice_wanted.get()
        && snapshot.connection.is_ready()
        && ui.voice_room_id.borrow().as_deref() == Some(room.room.id.as_str())
    {
        ui.join_voice();
        return;
    }
    ui.join_voice.set_is_visible(voice_room && !active);
    ui.leave_voice.set_is_visible(active);
    ui.mute_mic.set_is_visible(active);
    ui.mute_speaker.set_is_visible(active);
    ui.share.set_is_visible(active);
    ui.share_source.set_is_visible(active);
    ui.share_quality.set_is_visible(active);
    ui.file_panel.set_is_visible(active);
    ui.file_card.set_is_visible(active);
    ui.share_card.set_is_visible(active);
    if active {
        ui.refresh_file_members(room);
    }
    ui.file_cancel
        .set_is_visible(ui.file_active.borrow().is_some());
    ui.file_open.set_is_visible(ui.file_last.borrow().is_some());
    let file_text = ui.file_line.borrow().clone();
    if *ui.file_shown.borrow() != file_text {
        *ui.file_shown.borrow_mut() = file_text.clone();
        ui.file_status.set_text(Some(&file_text));
    }

    let presenting = room
        .members
        .iter()
        .any(|member| member.sharing && member.session_id != room.self_member.session_id);
    let controlling = ui.controlling();
    let hosting = ui.control_host_peer.borrow().is_some();
    let pending = ui.control_pending.get();
    let requested = ui.control_request.borrow().is_some();
    ui.request_control
        .set_is_visible(active && presenting && !controlling && !hosting && !pending && !requested);
    ui.stop_control
        .set_is_visible(active && (controlling || hosting || pending));
    ui.allow_control.set_is_visible(requested && active);
    ui.deny_control.set_is_visible(requested && active);

    let control_text = if let Some((peer, _)) = ui.control_request.borrow().as_ref() {
        format!("{} requests remote control", ui.member_name(peer))
    } else if let Some(until) = ui.control_until.get() {
        let peer = ui.control_peer.borrow().clone().unwrap_or_default();
        format!(
            "controlling {} \u{2014} {}",
            ui.member_name(&peer),
            countdown(until.saturating_duration_since(Instant::now()).as_secs())
        )
    } else if let Some(until) = ui.control_host_until.get() {
        let peer = ui.control_host_peer.borrow().clone().unwrap_or_default();
        format!(
            "{} is controlling \u{2014} {}",
            ui.member_name(&peer),
            countdown(until.saturating_duration_since(Instant::now()).as_secs())
        )
    } else if pending {
        "waiting for approval".to_owned()
    } else {
        String::new()
    };
    if *ui.control_line.borrow() != control_text {
        *ui.control_line.borrow_mut() = control_text.clone();
        ui.control_status.set_text(Some(&control_text));
    }

    let note = ui
        .voice_note
        .borrow()
        .clone()
        .unwrap_or_else(|| "off \u{2014} join to talk".to_owned());
    let line = if voice_room || active {
        format!("voice: {note}")
    } else {
        String::new()
    };
    if *ui.voice_line.borrow() != line {
        *ui.voice_line.borrow_mut() = line.clone();
        ui.voice_status.set_text(Some(&line));
    }
}

fn render_video(ui: &Rc<Ui>, presenting: bool) {
    if !presenting {
        if ui.last_video.borrow_mut().take().is_some() {
            *ui.voice_note.borrow_mut() = Some("screen sharing stopped".to_owned());
        }
        ui.show_video(false);
        return;
    }
    let sink = ui
        .voice
        .borrow()
        .as_ref()
        .map(|runtime| runtime.controller.video());
    let mut fresh = false;
    if let Some(sink) = sink
        && let Some(frame) = sink.take()
    {
        let mut surface = ui.surface.borrow_mut();
        if surface
            .publish(frame.width, frame.height, &frame.bgra)
            .is_ok()
        {
            ui.presenter.present_latest();
            let first = ui.last_video.borrow().is_none();
            *ui.last_video.borrow_mut() = Some(Instant::now());
            if first {
                *ui.voice_note.borrow_mut() =
                    Some(format!("receiving screen {}x{}", frame.width, frame.height));
            }
            fresh = true;
        }
    }
    let recent = ui
        .last_video
        .borrow()
        .is_some_and(|at| at.elapsed() < Duration::from_millis(1500));
    ui.show_video(presenting && (fresh || recent));
}
fn speaking(levels: &HashMap<String, (f32, Instant)>, id: &str) -> bool {
    levels
        .get(id)
        .is_some_and(|(level, at)| at.elapsed() < Duration::from_millis(500) && *level > 0.012)
}

fn smoke_request(settings: &ClientSettings) -> Option<ConnectRequest> {
    let host = std::env::var("RUST_CHAT_SMOKE_HOST").ok()?;
    let port = std::env::var("RUST_CHAT_SMOKE_PORT").ok()?.parse().ok()?;
    let password = std::env::var("RUST_CHAT_SMOKE_PASSWORD").ok()?;
    if host.is_empty() || password.is_empty() {
        return None;
    }
    let name = std::env::var("RUST_CHAT_SMOKE_NAME").unwrap_or_else(|_| {
        if settings.name.is_empty() {
            "smoke".to_owned()
        } else {
            settings.name.clone()
        }
    });
    let tls = std::env::var("RUST_CHAT_SMOKE_TLS")
        .map(|value| value != "0")
        .unwrap_or(false);
    Some(ConnectRequest {
        host,
        port,
        tls,
        name,
        password,
        room: None,
    })
}

fn text(value: &str, size: f64) -> Ref<TextBlock> {
    let block = TextBlock::new();
    block.set_text(Some(value));
    block.set_font_size(size);
    block
}

fn row() -> Ref<StackPanel> {
    let panel = StackPanel::new();
    panel.set_orientation(Orientation::Horizontal);
    panel.set_spacing(8.0);
    panel.set_vertical_alignment(VerticalAlignment::Center);
    panel
}

fn row_with(label: &str, field: Ref<TextBox>) -> Ref<StackPanel> {
    let panel = row();
    panel.children().add(text(label, 13.0));
    panel.children().add(field);
    panel
}

fn main() -> std::process::ExitCode {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("the client runtime starts");
    let memory = ConnectionMemory::new();
    let (settings, password) = memory.load();
    let client = ClientHandle::spawn(settings.clone(), runtime.handle());
    STARTUP.with(|cell| {
        *cell.borrow_mut() = Some(Startup {
            settings,
            password,
            client,
            memory,
            runtime: runtime.handle().clone(),
        });
    });

    let args: Vec<String> = std::env::args().skip(1).collect();
    let exit_code = AppBuilder::configure::<App>()
        .use_platform_detect()
        .start_with_classic_desktop_lifetime(&args);
    std::process::ExitCode::from(exit_code as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_formats_both_styles() {
        assert_eq!(format_clock(0, 5, true), "00:05");
        assert_eq!(format_clock(13, 40, true), "13:40");
        assert_eq!(format_clock(0, 5, false), "12:05 AM");
        assert_eq!(format_clock(11, 59, false), "11:59 AM");
        assert_eq!(format_clock(12, 0, false), "12:00 PM");
        assert_eq!(format_clock(23, 7, false), "11:07 PM");
    }

    #[test]
    fn device_lists_start_with_the_system_default() {
        let choices = device_choices(vec![
            rust_chat_media::AudioDevice {
                id: "input:0".into(),
                name: "Microphone".into(),
                is_default: true,
            },
            rust_chat_media::AudioDevice {
                id: "input:1".into(),
                name: "Headset".into(),
                is_default: false,
            },
        ]);
        assert_eq!(choices.len(), 3);
        assert_eq!(choices[0].id, None);
        assert_eq!(choices[0].label, "System default");
        assert_eq!(choices[1].label, "Microphone (default)");
        assert_eq!(choices[2].id.as_deref(), Some("input:1"));

        assert_eq!(device_index(&choices, None), 0);
        assert_eq!(device_index(&choices, Some("input:1")), 2);
        // A device that disappeared since the last run falls back to the default.
        assert_eq!(device_index(&choices, Some("input:9")), 0);
    }
}
