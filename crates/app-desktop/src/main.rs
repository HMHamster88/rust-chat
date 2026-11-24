//! FerroUI desktop shell: connect/trust, rooms, chat, local history and voice.

#![cfg(windows)]

use rust_chat_desktop::voice;

use ferroui_base::collections::FerroList;
use ferroui_base::layout::{HorizontalAlignment, Orientation, VerticalAlignment};
use ferroui_base::threading::{DispatcherPriority, DispatcherTimer};
use ferroui_base::{FerroObjectImpl, Ref, Thickness, ferro_class, ferro_impl_classes, instantiate};
use ferroui_controls::{
    AppBuilder, Application, ApplicationImpl, ApplicationImplExt, Button, CheckBox, Control,
    ItemsSource, ListBox, NewApplication, StackPanel, TextBlock, TextBox, Window,
};
use ferroui_desktop::AppBuilderDesktopExtensions;
use ferroui_themes_fluent::FluentTheme;
use rust_chat_client_core::{
    ChatSnapshot, ClientHandle, ClientSettings, ConnectRequest, ConnectionMemory, ConnectionState,
};
use rust_chat_media_desktop::audio::AudioIo;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};
use voice::{VoiceCommand, VoiceController, VoiceEvent};

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

struct VoiceRuntime {
    audio: AudioIo,
    controller: VoiceController,
    mic_muted: Cell<bool>,
    speaker_muted: Cell<bool>,
    room: String,
    /// Levels keyed by session id; the empty key is the local microphone.
    levels: RefCell<HashMap<String, (f32, Instant)>>,
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
    voice_status: Ref<TextBlock>,
    voice_note: RefCell<Option<String>>,
    voice_line: RefCell<String>,
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
        let controller = voice::start(self.client.clone(), self_id, handle, &self.runtime);
        *self.voice.borrow_mut() = Some(VoiceRuntime {
            audio,
            controller,
            mic_muted: Cell::new(true),
            speaker_muted: Cell::new(false),
            room: room.room.id.clone(),
            levels: RefCell::new(HashMap::new()),
            members_signature: RefCell::new(String::new()),
        });
        // Muted on join, like a push-to-talk call.
        let _ = self.client.set_voice(true);
        let _ = self.client.set_presence(Some(true), Some(false), None);
        *self.voice_note.borrow_mut() = Some("joining (microphone muted)".to_owned());
        refresh_voice(self, &snapshot);
    }

    fn leave_voice(&self, reason: Option<&str>) {
        let taken = self.voice.borrow_mut().take();
        if let Some(mut runtime) = taken {
            runtime.controller.send(VoiceCommand::Stop);
            runtime.audio.stop();
            let _ = self.client.set_voice(false);
            let _ = self.client.set_presence(Some(false), Some(false), None);
            *self.voice_note.borrow_mut() = Some(reason.unwrap_or("off").to_owned());
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
    let voice_status = text("voice: off", 13.0);
    let voice_row = row();
    voice_row.children().add(join_voice.clone());
    voice_row.children().add(leave_voice.clone());
    voice_row.children().add(mute_mic.clone());
    voice_row.children().add(mute_speaker.clone());
    voice_row.children().add(voice_status.clone());
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
    room_panel.children().add(text("ROOMS", 12.0));
    room_panel.children().add(rooms_panel.clone());
    room_panel.children().add(text("MEMBERS", 12.0));
    room_panel.children().add(members.clone());
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
        voice_status,
        voice_note: RefCell::new(None),
        voice_line: RefCell::new(String::new()),
        voice: RefCell::new(None),
    });

    wire(&ui);
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
        ui.leave_voice(Some(reason));
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
                    Some(VoiceEvent::Ready) => {
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
    ui.join_voice.set_is_visible(voice_room && !active);
    ui.leave_voice.set_is_visible(active);
    ui.mute_mic.set_is_visible(active);
    ui.mute_speaker.set_is_visible(active);

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
