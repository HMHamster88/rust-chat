//! FerroUI desktop shell: connect/trust, rooms, chat and local history.

#![cfg(windows)]

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
    ClientHandle, ClientSettings, ConnectRequest, ConnectionMemory, ConnectionState,
};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

struct Startup {
    settings: ClientSettings,
    password: Option<String>,
    client: ClientHandle,
    memory: ConnectionMemory,
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

struct Ui {
    client: ClientHandle,
    memory: ConnectionMemory,
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
            move || {
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
                let _ = ui.client.disconnect();
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

    if let Some(room) = &snapshot.room {
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

        let mut members = String::new();
        for member in &room.members {
            if !members.is_empty() {
                members.push_str("   ·   ");
            }
            members.push_str(&member.name);
            if member.session_id == room.self_member.session_id {
                members.push_str(" (you)");
            }
            if member.self_mute {
                members.push_str(" [muted]");
            }
            if member.sharing {
                members.push_str(" [presenting]");
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
        });
    });

    let args: Vec<String> = std::env::args().skip(1).collect();
    let exit_code = AppBuilder::configure::<App>()
        .use_platform_detect()
        .start_with_classic_desktop_lifetime(&args);
    std::process::ExitCode::from(exit_code as u8)
}
