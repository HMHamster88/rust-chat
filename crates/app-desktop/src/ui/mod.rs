//! The desktop shell: state, actions and the three views (connect, room, settings).

use ferroui_base::input::{
    InputElement, KeyEventArgs, PointerEventArgs, PointerPressedEventArgs,
    PointerReleasedEventArgs, PointerWheelEventArgs,
};
use ferroui_base::interactivity::RoutedEventHandlerToken;
use ferroui_base::layout::{HorizontalAlignment, Orientation, VerticalAlignment};
use ferroui_base::media::Brushes;
use ferroui_base::threading::{DispatcherPriority, DispatcherTimer};
use ferroui_base::{CornerRadius, Ref, Thickness};
use ferroui_controls::primitives::ScrollBarVisibility;
use ferroui_controls::{
    Border, Button, Canvas, CheckBox, ColumnDefinition, ColumnDefinitions, Control, Grid,
    GridLength, GridUnitType, RowDefinition, RowDefinitions, ScrollViewer, StackPanel, TextBlock,
    TextBox, Window,
};
use ferroui_media::MediaSubscription;
use ferroui_media_controls::TexturePresenter;
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

use crate::clock::local_clock;
use crate::icons;
use crate::platform::{open_path, remote_button, virtual_key};
use crate::ui_kit;

pub(crate) mod build;
mod refresh;
mod settings;
mod wire;

pub(crate) use build::create_main_window;

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
type UpdateResult = Result<Option<update::Release>, String>;

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
    pub(crate) client: ClientHandle,
    pub(crate) memory: ConnectionMemory,
    pub(crate) runtime: tokio::runtime::Handle,
    pub(crate) settings: RefCell<ClientSettings>,
    pub(crate) connect_view: Ref<Grid>,
    pub(crate) room_view: Ref<Grid>,
    pub(crate) stage_frame: Ref<Border>,
    pub(crate) file_card: Ref<Border>,
    pub(crate) tip_layer: Ref<Canvas>,
    pub(crate) tip_bubble: Ref<Border>,
    pub(crate) tip_label: Ref<TextBlock>,
    pub(crate) server_line: Ref<TextBlock>,
    pub(crate) settings_nav: Ref<Button>,
    pub(crate) disconnect_nav: Ref<Button>,
    pub(crate) host: Ref<TextBox>,
    pub(crate) port: Ref<TextBox>,
    pub(crate) name: Ref<TextBox>,
    pub(crate) password: Ref<TextBox>,
    pub(crate) tls: Ref<CheckBox>,
    pub(crate) remember: Ref<CheckBox>,
    pub(crate) connect_status: Ref<TextBlock>,
    pub(crate) error_text: Ref<TextBlock>,
    pub(crate) trust_panel: Ref<StackPanel>,
    pub(crate) trust_text: Ref<TextBlock>,
    pub(crate) room_title: Ref<TextBlock>,
    pub(crate) room_status: Ref<TextBlock>,
    pub(crate) members_panel: Ref<StackPanel>,
    pub(crate) members_sig: RefCell<String>,
    pub(crate) rooms_panel: Ref<StackPanel>,
    pub(crate) feed_panel: Ref<StackPanel>,
    pub(crate) feed_scroll: Ref<ScrollViewer>,
    pub(crate) composer: Ref<TextBox>,
    pub(crate) last_rooms: RefCell<String>,
    pub(crate) last_messages: RefCell<String>,
    pub(crate) connect_button: Ref<Button>,
    pub(crate) approve: Ref<Button>,
    pub(crate) deny: Ref<Button>,
    pub(crate) send: Ref<Button>,
    pub(crate) forget: Ref<Button>,
    pub(crate) disconnect: Ref<Button>,
    pub(crate) join_voice: Ref<Button>,
    pub(crate) leave_voice: Ref<Button>,
    pub(crate) mute_mic: Ref<Button>,
    pub(crate) mute_speaker: Ref<Button>,
    pub(crate) share: Ref<Button>,
    pub(crate) share_source: Ref<Button>,
    pub(crate) share_quality: Ref<Button>,
    pub(crate) share_sources: RefCell<Vec<SourceInfo>>,
    pub(crate) share_source_index: Cell<usize>,
    pub(crate) share_preset_index: Cell<usize>,
    pub(crate) request_control: Ref<Button>,
    pub(crate) stop_control: Ref<Button>,
    pub(crate) allow_control: Ref<Button>,
    pub(crate) deny_control: Ref<Button>,
    pub(crate) control_status: Ref<TextBlock>,
    pub(crate) control_pending: Cell<bool>,
    pub(crate) control_peer: RefCell<Option<String>>,
    pub(crate) control_until: Cell<Option<Instant>>,
    pub(crate) control_host_peer: RefCell<Option<String>>,
    pub(crate) control_host_until: Cell<Option<Instant>>,
    pub(crate) control_request: RefCell<Option<(String, u64)>>,
    pub(crate) last_mouse: Cell<Instant>,
    pub(crate) input_tokens: RefCell<Vec<RoutedEventHandlerToken>>,
    pub(crate) control_line: RefCell<String>,
    pub(crate) file_members: Ref<StackPanel>,
    pub(crate) file_path: Ref<TextBox>,
    pub(crate) file_caption: Ref<TextBox>,
    pub(crate) file_send: Ref<Button>,
    pub(crate) file_choose: Ref<Button>,
    pub(crate) file_offer_panel: Ref<StackPanel>,
    pub(crate) file_offer_text: Ref<TextBlock>,
    pub(crate) file_accept: Ref<Button>,
    pub(crate) file_decline: Ref<Button>,
    pub(crate) file_status: Ref<TextBlock>,
    pub(crate) file_cancel: Ref<Button>,
    pub(crate) file_open: Ref<Button>,
    pub(crate) file_panel: Ref<StackPanel>,
    pub(crate) file_target: RefCell<Option<String>>,
    pub(crate) file_offers: RefCell<Vec<(String, String, String, u64)>>,
    pub(crate) file_names: RefCell<HashMap<String, String>>,
    pub(crate) file_active: RefCell<Option<String>>,
    pub(crate) file_last: RefCell<Option<PathBuf>>,
    pub(crate) file_members_sig: RefCell<String>,
    pub(crate) file_line: RefCell<String>,
    pub(crate) file_shown: RefCell<String>,
    pub(crate) update_status: Ref<TextBlock>,
    pub(crate) update_check: Ref<Button>,
    pub(crate) update_open: Ref<Button>,
    pub(crate) update_auto: Ref<CheckBox>,
    pub(crate) update_rx: RefCell<Option<Receiver<UpdateResult>>>,
    pub(crate) update_tx: Sender<UpdateResult>,
    pub(crate) update_url: RefCell<Option<String>>,
    pub(crate) presenter: Ref<TexturePresenter>,
    pub(crate) video_host: Ref<Grid>,
    pub(crate) surface: Rc<RefCell<VideoSurface>>,
    pub(crate) _presenter_sub: RefCell<Option<MediaSubscription>>,
    pub(crate) last_video: RefCell<Option<Instant>>,
    pub(crate) video_visible: Cell<bool>,
    pub(crate) video_label: Ref<TextBlock>,
    pub(crate) voice_status: Ref<TextBlock>,
    pub(crate) voice_note: RefCell<Option<String>>,
    pub(crate) voice_line: RefCell<String>,
    pub(crate) voice_wanted: Cell<bool>,
    pub(crate) voice_room_id: RefCell<Option<String>>,
    pub(crate) voice: RefCell<Option<VoiceRuntime>>,
    pub(crate) settings_screen: SettingsScreen,
    pub(crate) file_prefs: std::sync::Arc<FilePrefs>,
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

    /// Shows the hover hint at a point measured in the tooltip layer's coordinates.
    fn show_tip(&self, label: &str, at: ferroui_base::Point) {
        self.tip_label.set_text(Some(label));
        let bounds = self.tip_layer.bounds();
        let max_x = (bounds.right() - 130.0).max(0.0);
        let max_y = (bounds.bottom() - 40.0).max(0.0);
        Canvas::set_left(&self.tip_bubble, (at.x + 14.0).min(max_x));
        Canvas::set_top(&self.tip_bubble, (at.y + 18.0).min(max_y));
        self.tip_bubble.set_is_visible(true);
    }

    fn hide_tip(&self) {
        self.tip_bubble.set_is_visible(false);
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
            .set_text(Some(&format!("Rust Chat {APP_VERSION}")));
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
    about_slot: Ref<StackPanel>,
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

pub(crate) fn text(value: &str, size: f64) -> Ref<TextBlock> {
    let block = TextBlock::new();
    block.set_text(Some(value));
    block.set_font_size(size);
    block
}

pub(crate) fn row() -> Ref<StackPanel> {
    let panel = StackPanel::new();
    panel.set_orientation(Orientation::Horizontal);
    panel.set_spacing(6.0);
    panel.set_vertical_alignment(VerticalAlignment::Center);
    panel
}

#[cfg(test)]
mod tests {
    use super::*;

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
