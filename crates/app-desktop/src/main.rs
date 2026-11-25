//! FerroUI desktop shell: connect/trust, rooms, chat, history, voice, video, files and control.

#![cfg(windows)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod clock;
mod icons;
mod platform;
mod ui;
mod ui_kit;

use ferroui_base::threading::DispatcherTimer;
use ferroui_base::{FerroObjectImpl, Ref, ferro_class, ferro_impl_classes, instantiate};
use ferroui_controls::{
    AppBuilder, Application, ApplicationImpl, ApplicationImplExt, NewApplication,
};
use ferroui_desktop::AppBuilderDesktopExtensions;
use ferroui_themes_fluent::FluentTheme;
use rust_chat_client_core::{ClientHandle, ClientSettings, ConnectionMemory};
use std::cell::RefCell;
use std::rc::Rc;

pub(crate) struct Startup {
    settings: ClientSettings,
    password: Option<String>,
    client: ClientHandle,
    memory: ConnectionMemory,
    runtime: tokio::runtime::Handle,
}

thread_local! {
    pub(crate) static STARTUP: RefCell<Option<Startup>> = const { RefCell::new(None) };
    pub(crate) static TIMER: RefCell<Option<Rc<DispatcherTimer>>> = const { RefCell::new(None) };
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
            desktop.set_main_window(Some(ui::create_main_window()));
        }
        Self::parent_on_framework_initialization_completed(this);
    }
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
