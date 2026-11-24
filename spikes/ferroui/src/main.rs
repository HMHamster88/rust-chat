//! M0 FerroUI gate: builds the minimal facade application and exits after
//! `FERROUI_SMOKE_EXIT_MS`, so RSS/CPU can be measured without a person at the screen.

use ferroui::base::threading::{DispatcherPriority, DispatcherTimer};
use ferroui::prelude::*;
use std::rc::Rc;
use std::time::Duration;

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
        this.set_name(Some("FerroUI spike".to_string()));
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

fn create_main_window() -> Ref<Window> {
    let text = TextBlock::new();
    text.set_text(Some("FerroUI M0 gate"));
    text.set_font_size(24.0);

    let clicks = Rc::new(std::cell::Cell::new(0u32));
    let button = Button::new();
    button.set_content(Some(Rc::new("Click me".to_string())));
    button.click({
        let text = text.clone();
        move |_, _| {
            clicks.set(clicks.get() + 1);
            text.set_text(Some(format!("Clicked {} times", clicks.get()).as_str()));
        }
    });

    let panel = StackPanel::new();
    panel.set_spacing(12.0);
    panel.set_horizontal_alignment(HorizontalAlignment::Center);
    panel.set_vertical_alignment(VerticalAlignment::Center);
    panel.children().add(text);
    panel.children().add(button);

    let window = Window::new();
    window.set_title(Some("FerroUI spike".to_string()));
    window.set_width(900.0);
    window.set_height(620.0);
    window.set_padding(Thickness::uniform(16.0));
    window.set_content(Some(Control::boxed(panel)));

    window.opened(|| println!("window opened")).detach();
    window.closed(|| println!("window closed")).detach();

    if let Some(ms) = std::env::var("FERROUI_SMOKE_EXIT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
    {
        println!("closing after {ms} ms");
        let window = window.clone();
        let _timer = DispatcherTimer::run_once(
            move || {
                window.close();
            },
            Duration::from_millis(ms),
            DispatcherPriority::NORMAL,
        )
        .detach();
    }

    window
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let exit_code = AppBuilder::configure::<App>()
        .use_platform_detect()
        .start_with_classic_desktop_lifetime(&args);
    std::process::ExitCode::from(exit_code as u8)
}
