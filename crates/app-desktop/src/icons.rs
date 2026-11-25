//! MDI icon font plus the small helpers that put glyphs into buttons and rows.
//!
//! Some helpers are here for the upcoming layout pass; they are unused until then.
//!
//! The subset TTF is bundled (`assets/MaterialDesignIcons-subset.ttf`) and registered with the
//! FerroUI font manager from memory, so no font installation or file on disk is needed.

#![allow(dead_code)]

use ferroui_base::Ref;
use ferroui_base::media::{FontFamily, FontManager, FontSimulations};
use ferroui_controls::automation::AutomationProperties;
use ferroui_controls::{Button, StackPanel, TextBlock};
use std::cell::RefCell;
use std::rc::Rc;

const FONT: &[u8] = include_bytes!("../assets/MaterialDesignIcons-subset.ttf");
const FAMILY_NAME: &str = "Material Design Icons";

/// The icons the UI uses. The value is the Material Design Icons codepoint.
///
/// The list is a superset of what the shell uses today so the layout pass can pick from it.
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Settings,
    Close,
    Disconnect,
    Phone,
    PhoneOff,
    Microphone,
    MicrophoneOff,
    Speaker,
    SpeakerOff,
    ShareScreen,
    Send,
    Attach,
    Folder,
    File,
    Members,
    Chat,
    Server,
    Shield,
    Delete,
    Info,
    History,
    RemoteDesktop,
    Check,
    ChevronRight,
    ChevronDown,
    OpenExternal,
    Download,
    Upload,
    Video,
    Keyboard,
    RemoteControl,
    Restart,
    Palette,
    Signal,
}

impl Icon {
    pub fn glyph(self) -> char {
        let code = match self {
            Icon::Settings => 0xF0493,
            Icon::Close => 0xF0156,
            Icon::Disconnect => 0xF0425,
            Icon::Phone => 0xF03F2,
            Icon::PhoneOff => 0xF0DEF,
            Icon::Microphone => 0xF036C,
            Icon::MicrophoneOff => 0xF036D,
            Icon::Speaker => 0xF057E,
            Icon::SpeakerOff => 0xF0581,
            Icon::ShareScreen => 0xF1483,
            Icon::Send => 0xF048A,
            Icon::Attach => 0xF03E2,
            Icon::Folder => 0xF0256,
            Icon::File => 0xF09EE,
            Icon::Members => 0xF0B58,
            Icon::Chat => 0xF036A,
            Icon::Server => 0xF048B,
            Icon::Shield => 0xF0CC8,
            Icon::Delete => 0xF09E7,
            Icon::Info => 0xF02FD,
            Icon::History => 0xF02DA,
            Icon::RemoteDesktop => 0xF08B9,
            Icon::Check => 0xF012C,
            Icon::ChevronRight => 0xF0142,
            Icon::ChevronDown => 0xF0140,
            Icon::OpenExternal => 0xF03CC,
            Icon::Download => 0xF01DA,
            Icon::Upload => 0xF0552,
            Icon::Video => 0xF0567,
            Icon::Keyboard => 0xF097B,
            Icon::RemoteControl => 0xF0CFE,
            Icon::Restart => 0xF0709,
            Icon::Palette => 0xF0E0C,
            Icon::Signal => 0xF04A2,
        };
        char::from_u32(code).unwrap_or('\u{FFFD}')
    }
}

thread_local! {
    static FAMILY: RefCell<Option<FontFamily>> = const { RefCell::new(None) };
}

/// The icon font family, registered on first use.
pub fn family() -> FontFamily {
    FAMILY.with(|cell| {
        if let Some(family) = cell.borrow().as_ref() {
            return family.clone();
        }
        // Registering the face from memory is what makes the family resolvable: the Direct2D
        // backend only knows system fonts and faces handed to it this way.
        let mut cursor = FONT;
        let loaded = FontManager::current()
            .platform_impl()
            .try_create_glyph_typeface_from_stream(&mut cursor, FontSimulations::None);
        if loaded.is_none() {
            eprintln!("warning: the bundled icon font could not be registered");
        }
        let family = FontFamily::new(FAMILY_NAME);
        *cell.borrow_mut() = Some(family.clone());
        family
    })
}

/// A glyph drawn at `size` points.
pub fn icon(glyph: Icon, size: f64) -> Ref<TextBlock> {
    let block = TextBlock::new();
    block.set_font_family(family());
    block.set_font_size(size);
    let mut text = String::new();
    text.push(glyph.glyph());
    block.set_text(Some(&text));
    block
}

/// A square button that shows only the glyph, with an automation name. Hover tooltips are wired
/// from the registry below, because they need the window's tooltip layer.
pub fn icon_button(glyph: Icon, name: &str) -> Ref<Button> {
    let button = Button::new();
    let content = icon(glyph, 16.0);
    content.set_horizontal_alignment(ferroui_base::layout::HorizontalAlignment::Center);
    content.set_vertical_alignment(ferroui_base::layout::VerticalAlignment::Center);
    button.set_content(Some(Rc::new(content) as ferroui_base::BoxedValue));
    button.set_width(32.0);
    button.set_height(32.0);
    button.set_padding(ferroui_base::Thickness::uniform(0.0));
    button.set_horizontal_content_alignment(ferroui_base::layout::HorizontalAlignment::Center);
    button.set_vertical_content_alignment(ferroui_base::layout::VerticalAlignment::Center);
    AutomationProperties::set_name(&button, Some(name));
    AutomationProperties::set_help_text(&button, Some(name));
    BUTTONS.with(|buttons| buttons.borrow_mut().push((button.clone(), name.to_owned())));
    button
}

thread_local! {
    /// Icon buttons created so far, so the window can attach hover tooltips after building the tree.
    static BUTTONS: RefCell<Vec<(Ref<Button>, String)>> = const { RefCell::new(Vec::new()) };
}

/// Drains the registered icon buttons (called once, when the window is wired).
pub fn take_buttons() -> Vec<(Ref<Button>, String)> {
    BUTTONS.with(|buttons| std::mem::take(&mut *buttons.borrow_mut()))
}

/// Replaces a button's content with a centred glyph, keeping an icon-only button icon-only when
/// its state changes.
pub fn set_icon(button: &Button, glyph: Icon) {
    let content = icon(glyph, 16.0);
    content.set_horizontal_alignment(ferroui_base::layout::HorizontalAlignment::Center);
    content.set_vertical_alignment(ferroui_base::layout::VerticalAlignment::Center);
    button.set_content(Some(Rc::new(content) as ferroui_base::BoxedValue));
}

/// A button face with the glyph and a text label, for actions that stay readable.
pub fn icon_label(glyph: Icon, label: &str) -> Ref<StackPanel> {
    let row = StackPanel::new();
    row.set_orientation(ferroui_base::layout::Orientation::Horizontal);
    row.set_spacing(7.0);
    row.children().add(icon(glyph, 15.0));
    row.children().add(crate::ui::text(label, 13.0));
    row
}

/// Sets an automation name without an icon, for text-only buttons that stay icon-free.
pub fn name_only(button: &Button, name: &str) {
    AutomationProperties::set_name(button, Some(name));
}
