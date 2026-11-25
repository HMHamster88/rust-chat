//! Thin Win32 helpers: work area, DPI, opening files and translating input.

use ferroui_base::input::Key;
use rust_chat_desktop::control::RemoteButton;

#[derive(Clone, Copy)]
pub(crate) struct WorkArea {
    pub(crate) left: i32,
    pub(crate) top: i32,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

pub(crate) fn work_area() -> Option<WorkArea> {
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
pub(crate) fn system_scale() -> f64 {
    use windows::Win32::UI::HiDpi::GetDpiForSystem;
    let dpi = unsafe { GetDpiForSystem() } as f64;
    if dpi > 0.0 { dpi / 96.0 } else { 1.0 }
}

/// Opens a file or URL with its default handler.
pub(crate) fn open_path(target: &str) {
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", target])
        .spawn();
}

pub(crate) fn remote_button(button: ferroui_base::input::MouseButton) -> Option<RemoteButton> {
    match button {
        ferroui_base::input::MouseButton::Left => Some(RemoteButton::Left),
        ferroui_base::input::MouseButton::Right => Some(RemoteButton::Right),
        ferroui_base::input::MouseButton::Middle => Some(RemoteButton::Middle),
        _ => None,
    }
}

/// WPF-style `Key` values to Windows virtual-key codes, with `VkKeyScanW` for printable keys.
pub(crate) fn virtual_key(key: Key, symbol: Option<&str>) -> Option<u16> {
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
