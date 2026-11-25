//! FerroUI desktop shell: connect/trust, rooms, chat, local history and voice.

#![cfg(windows)]

use ferroui_base::collections::FerroList;
use ferroui_base::input::{
    InputElement, Key, KeyEventArgs, PointerEventArgs, PointerPressedEventArgs,
    PointerReleasedEventArgs, PointerWheelEventArgs,
};
use ferroui_base::interactivity::RoutedEventHandlerToken;
use ferroui_base::layout::{HorizontalAlignment, Orientation, VerticalAlignment};
use ferroui_base::media::Brushes;
use ferroui_base::threading::{DispatcherPriority, DispatcherTimer};
use ferroui_base::{FerroObjectImpl, Ref, Thickness, ferro_class, ferro_impl_classes, instantiate};
use ferroui_controls::{
    AppBuilder, Application, ApplicationImpl, ApplicationImplExt, Button, CheckBox, Control, Grid,
    ItemsSource, ListBox, NewApplication, StackPanel, TextBlock, TextBox, Window,
};
use ferroui_desktop::AppBuilderDesktopExtensions;
use ferroui_media::MediaSubscription;
use ferroui_media_controls::TexturePresenter;
use ferroui_themes_fluent::FluentTheme;
use rust_chat_client_core::{
    ChatSnapshot, ClientHandle, ClientSettings, ConnectRequest, ConnectionMemory, ConnectionState,
};
use rust_chat_desktop::control::{DEFAULT_LEASE_SECS, InputEvent, RemoteButton, countdown};
use rust_chat_desktop::video_surface::{VideoSurface, build_presenter};
use rust_chat_desktop::voice::{self, VoiceCommand, VoiceController, VoiceEvent};
use rust_chat_media_desktop::audio::AudioIo;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

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
    room_panel: Ref<StackPanel>,
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
    members: Ref<TextBlock>,
    rooms_panel: Ref<StackPanel>,
    message_model: Rc<FerroList<String>>,
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
}

impl Ui {
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
        if let Err(error) = self.memory.save(
            &settings,
            if remember {
                Some(password.as_str())
            } else {
                None
            },
        ) {
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
        let (audio, handle) = match AudioIo::start(None, None) {
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
    let forget = Button::new();
    forget.set_content(Some(Rc::new("Forget pinned key".to_string())));
    let connect_status = text("not connected", 14.0);
    let error_text = text("", 13.0);

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
    connect_panel.children().add(actions);
    connect_panel.children().add(connect_status.clone());
    connect_panel.children().add(trust_panel.clone());

    let room_title = text("", 20.0);
    let room_status = text("", 13.0);
    let members = text("", 13.0);
    let rooms_panel = StackPanel::new();
    rooms_panel.set_spacing(4.0);
    let join_voice = Button::new();
    join_voice.set_content(Some(Rc::new("Join voice".to_string())));
    let leave_voice = Button::new();
    leave_voice.set_content(Some(Rc::new("Leave voice".to_string())));
    let mute_mic = Button::new();
    mute_mic.set_content(Some(Rc::new("Unmute mic".to_string())));
    let mute_speaker = Button::new();
    mute_speaker.set_content(Some(Rc::new("Mute speakers".to_string())));
    let share = Button::new();
    share.set_content(Some(Rc::new("Share screen".to_string())));
    let request_control = Button::new();
    request_control.set_content(Some(Rc::new("Request control".to_string())));
    request_control.set_is_visible(false);
    let stop_control = Button::new();
    stop_control.set_content(Some(Rc::new("Stop control".to_string())));
    stop_control.set_is_visible(false);
    let allow_control = Button::new();
    allow_control.set_content(Some(Rc::new("Allow 30 min".to_string())));
    allow_control.set_is_visible(false);
    let deny_control = Button::new();
    deny_control.set_content(Some(Rc::new("Deny".to_string())));
    deny_control.set_is_visible(false);
    let control_status = text("", 13.0);
    let control_row = row();
    control_row.children().add(request_control.clone());
    control_row.children().add(stop_control.clone());
    control_row.children().add(allow_control.clone());
    control_row.children().add(deny_control.clone());
    control_row.children().add(control_status.clone());
    let voice_status = text("voice: off", 13.0);
    let voice_row = row();
    voice_row.children().add(join_voice.clone());
    voice_row.children().add(leave_voice.clone());
    voice_row.children().add(mute_mic.clone());
    voice_row.children().add(mute_speaker.clone());
    voice_row.children().add(share.clone());
    voice_row.children().add(voice_status.clone());

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

    let message_model = Rc::new(FerroList::new());
    let message_list = ListBox::new();
    message_list.set_items_source(Some(ItemsSource::from(message_model.clone())));
    message_list.set_height(360.0);
    let composer = TextBox::new();
    composer.set_accepts_return(false);
    composer.set_width(760.0);
    let send = Button::new();
    send.set_content(Some(Rc::new("Send".to_string())));
    let composer_row = row();
    composer_row.children().add(composer.clone());
    composer_row.children().add(send.clone());
    let disconnect = Button::new();
    disconnect.set_content(Some(Rc::new("Disconnect".to_string())));
    let room_actions = row();
    room_actions.children().add(disconnect.clone());

    let room_panel = StackPanel::new();
    room_panel.set_spacing(10.0);
    let room_header = row();
    room_header.children().add(room_title.clone());
    room_header.children().add(room_status.clone());
    room_panel.children().add(room_header);
    room_panel.children().add(room_actions);
    room_panel.children().add(voice_row);
    room_panel.children().add(control_row);
    room_panel.children().add(text("ROOMS", 12.0));
    room_panel.children().add(rooms_panel.clone());
    room_panel.children().add(text("MEMBERS", 12.0));
    room_panel.children().add(members.clone());
    room_panel.children().add(video_label.clone());
    room_panel.children().add(video_host.clone());
    room_panel.children().add(message_list);
    room_panel.children().add(composer_row);
    room_panel.set_is_visible(false);

    let root = StackPanel::new();
    root.set_horizontal_alignment(HorizontalAlignment::Stretch);
    root.set_vertical_alignment(VerticalAlignment::Stretch);
    root.children().add(connect_panel.clone());
    root.children().add(room_panel.clone());
    root.children().add(error_text.clone());

    let ui = Rc::new(Ui {
        client: startup.client,
        memory: startup.memory,
        runtime: startup.runtime,
        settings: RefCell::new(settings.clone()),
        connect_panel,
        room_panel,
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
        members,
        rooms_panel,
        message_model,
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

    let window = Window::new();
    window.set_title(Some("rust-chat".to_string()));
    window.set_width(1100.0);
    window.set_height(800.0);
    window.set_padding(Thickness::uniform(18.0));
    window.set_content(Some(Control::boxed(root)));
    window
        .closed({
            let client = ui.client.clone();
            let weak = Rc::downgrade(&ui);
            move || {
                if let Some(ui) = weak.upgrade() {
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
    ui.room_panel.set_is_visible(room_visible);
    ui.connect_panel.set_is_visible(!room_visible);

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
        let mut members = String::new();
        for member in &room.members {
            if !members.is_empty() {
                members.push_str("   ·   ");
            }
            members.push_str(&member.name);
            let is_self = member.session_id == room.self_member.session_id;
            if is_self {
                members.push_str(" (you)");
            }
            if member.self_mute {
                members.push_str(" [muted]");
            }
            if member.sharing {
                members.push_str(" [presenting]");
            }
            if member.voice {
                members.push_str(" [voice]");
            }
            if let Some(label) = stats.get(&member.session_id)
                && !label.is_empty()
            {
                members.push_str(&format!(" ({label})"));
            }
            if speaking(&levels, if is_self { "" } else { &member.session_id }) {
                members.push_str(" \u{266a}");
            }
        }
        ui.members.set_text(Some(&members));

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
                let button = Button::new();
                button.set_content(Some(Rc::new(format!(
                    "{} ({})",
                    summary.name, summary.member_count
                ))));
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
        let messages_signature =
            format!("{}:{}:{}", room.room.id, snapshot.messages.len(), last_id);
        if *ui.last_messages.borrow() != messages_signature {
            *ui.last_messages.borrow_mut() = messages_signature;
            ui.message_model.clear();
            for message in snapshot.messages.iter().rev() {
                ui.message_model
                    .add(format!("{}: {}", message.name, message.text));
            }
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
