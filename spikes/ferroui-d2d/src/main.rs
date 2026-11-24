//! M0 FerroUI gate, native Windows backend (Direct2D/DirectWrite, no Skia/ANGLE/libclang).
//!
//! Exercises the three things the chat UI needs most: a 5,000-item virtualized list, a
//! multiline text box, and a layout that updates without rebuilding the tree.

use ferroui_base::collections::FerroList;
use ferroui_base::layout::{HorizontalAlignment, VerticalAlignment};
use ferroui_base::threading::{DispatcherPriority, DispatcherTimer};
use ferroui_base::{FerroObjectImpl, Ref, Thickness, ferro_class, ferro_impl_classes, instantiate};
use ferroui_controls::{
    AppBuilder, Application, ApplicationImpl, ApplicationImplExt, Button, Control, ItemsSource,
    ListBox, NewApplication, StackPanel, TextBlock, TextBox, Window,
};
use ferroui_desktop::AppBuilderDesktopExtensions;
use ferroui_themes_fluent::FluentTheme;
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

const MESSAGE_COUNT: usize = 5_000;

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
        this.set_name(Some("FerroUI D2D spike".to_string()));
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
    let header = TextBlock::new();
    header.set_text(Some("FerroUI M0 gate — 5,000-message list + multiline input"));
    header.set_font_size(18.0);

    let messages = Rc::new(FerroList::new());
    messages.add_range((0..MESSAGE_COUNT).map(|index| {
        format!(
            "#{index:05}  alice  ·  a message long enough to exercise text layout and wrapping, #{}",
            index % 7
        )
    }));

    let list = ListBox::new();
    list.set_items_source(Some(ItemsSource::from(messages.clone())));
    list.set_width(760.0);
    list.set_height(380.0);

    let input = TextBox::new();
    input.set_accepts_return(true);
    input.set_width(760.0);
    input.set_height(96.0);
    input.set_text(Some(
        "Привет, мир! 你好，世界。\nSecond line: emoji 🎉 and combining e\u{0301}.\nThird line for scrolling.",
    ));

    let status = TextBlock::new();
    status.set_text(Some(format!("{MESSAGE_COUNT} messages")));

    let next_index = Rc::new(Cell::new(MESSAGE_COUNT));
    let append = Button::new();
    append.set_content(Some(Rc::new("Append message".to_string())));
    append.click({
        let messages = messages.clone();
        let status = status.clone();
        let next_index = next_index.clone();
        move |_, _| {
            let index = next_index.get();
            next_index.set(index + 1);
            messages.insert(0, format!("#{index:05}  bob  ·  appended from the gate"));
            status.set_text(Some(format!("{} messages", index + 1)));
        }
    });

    let panel = StackPanel::new();
    panel.set_spacing(12.0);
    panel.set_padding(Thickness::uniform(16.0));
    panel.set_horizontal_alignment(HorizontalAlignment::Stretch);
    panel.set_vertical_alignment(VerticalAlignment::Stretch);
    panel.children().add(header);
    panel.children().add(list);
    panel.children().add(input);
    panel.children().add(append);
    panel.children().add(status);

    let window = Window::new();
    window.set_title(Some("FerroUI D2D spike".to_string()));
    window.set_width(900.0);
    window.set_height(700.0);
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
            move || window.close(),
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
