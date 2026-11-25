//! Remote-control input injection through `SendInput`.
//!
//! Coordinates arrive normalized to the shared monitor. Keys and mouse buttons are
//! tracked so ending a lease always releases everything it pressed.

use std::collections::HashSet;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP,
    MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN,
    MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE,
    MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL,
    MOUSEINPUT, SendInput, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

pub const VK_MENU: u16 = 0x12;
pub const VK_LMENU: u16 = 0xA4;
pub const VK_RMENU: u16 = 0xA5;
pub const VK_F4: u16 = 0x73;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

impl MouseButton {
    fn flags(self, pressed: bool) -> MOUSE_EVENT_FLAGS {
        match (self, pressed) {
            (Self::Left, true) => MOUSEEVENTF_LEFTDOWN,
            (Self::Left, false) => MOUSEEVENTF_LEFTUP,
            (Self::Right, true) => MOUSEEVENTF_RIGHTDOWN,
            (Self::Right, false) => MOUSEEVENTF_RIGHTUP,
            (Self::Middle, true) => MOUSEEVENTF_MIDDLEDOWN,
            (Self::Middle, false) => MOUSEEVENTF_MIDDLEUP,
        }
    }
}

/// A monitor rectangle in virtual-desktop pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Injects events for one granted controller. `dry_run` records nothing on screen.
pub struct InputInjector {
    rect: ScreenRect,
    held_keys: HashSet<u16>,
    held_buttons: HashSet<MouseButton>,
    alt_down: bool,
    dry_run: bool,
    /// Events that would have been injected (dry-run only).
    pub recorded: Vec<String>,
}

impl InputInjector {
    pub fn new(rect: ScreenRect) -> Self {
        Self {
            rect,
            held_keys: HashSet::new(),
            held_buttons: HashSet::new(),
            alt_down: false,
            dry_run: false,
            recorded: Vec::new(),
        }
    }

    /// Test mode: validate filters and mapping without touching the real session.
    pub fn dry_run(rect: ScreenRect) -> Self {
        let mut injector = Self::new(rect);
        injector.dry_run = true;
        injector
    }

    pub fn is_dry_run(&self) -> bool {
        self.dry_run
    }

    /// Normalized (0..1) position inside the shared monitor.
    pub fn mouse_move(&mut self, x: f32, y: f32) {
        let x = x.clamp(0.0, 1.0);
        let y = y.clamp(0.0, 1.0);
        let screen_x = self.rect.x as f32 + x * self.rect.width as f32;
        let screen_y = self.rect.y as f32 + y * self.rect.height as f32;
        if self.dry_run {
            self.recorded
                .push(format!("move {screen_x:.0},{screen_y:.0}"));
            return;
        }
        unsafe {
            let (vx, vy, vw, vh) = virtual_desktop();
            if vw <= 0 || vh <= 0 {
                return;
            }
            let dx = ((screen_x - vx as f32) * 65535.0 / vw as f32).round() as i32;
            let dy = ((screen_y - vy as f32) * 65535.0 / vh as f32).round() as i32;
            self.send_mouse(
                dx,
                dy,
                0,
                MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
            );
        }
    }

    pub fn mouse_button(&mut self, button: MouseButton, pressed: bool) {
        if self.dry_run {
            self.recorded.push(format!("button {button:?} {pressed}"));
            if pressed {
                self.held_buttons.insert(button);
            } else {
                self.held_buttons.remove(&button);
            }
            return;
        }
        if pressed {
            self.held_buttons.insert(button);
        } else {
            self.held_buttons.remove(&button);
        }
        unsafe {
            self.send_mouse(0, 0, 0, button.flags(pressed));
        }
    }

    pub fn wheel(&mut self, delta: i32, horizontal: bool) {
        if self.dry_run {
            self.recorded
                .push(format!("wheel {delta} horizontal={horizontal}"));
            return;
        }
        let flag = if horizontal {
            MOUSEEVENTF_HWHEEL
        } else {
            MOUSEEVENTF_WHEEL
        };
        unsafe {
            self.send_mouse(0, 0, delta, flag);
        }
    }

    /// Returns false when the key is filtered (Alt+F4).
    pub fn key(&mut self, vk: u16, pressed: bool) -> bool {
        if vk == VK_MENU || vk == VK_LMENU || vk == VK_RMENU {
            self.alt_down = pressed;
        }
        let alt = self.alt_down || matches!(vk, VK_LMENU | VK_RMENU);
        if pressed && vk == VK_F4 && alt {
            self.recorded.push("filtered Alt+F4".to_owned());
            return false;
        }
        if pressed {
            self.held_keys.insert(vk);
        } else {
            self.held_keys.remove(&vk);
        }
        if self.dry_run {
            self.recorded.push(format!("key {vk:#04x} {pressed}"));
            return true;
        }
        let mut flags = KEYBD_EVENT_FLAGS(0);
        if !pressed {
            flags |= KEYEVENTF_KEYUP;
        }
        unsafe {
            let input = INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VIRTUAL_KEY(vk),
                        wScan: 0,
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            };
            let _ = SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
        }
        true
    }

    /// Releases every key and button this injector pressed.
    pub fn release_all(&mut self) {
        let keys: Vec<u16> = self.held_keys.drain().collect();
        for key in keys {
            self.key(key, false);
        }
        let buttons: Vec<MouseButton> = self.held_buttons.drain().collect();
        for button in buttons {
            self.mouse_button(button, false);
        }
        self.alt_down = false;
    }

    unsafe fn send_mouse(&self, dx: i32, dy: i32, data: i32, flags: MOUSE_EVENT_FLAGS) {
        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    mouseData: data as u32,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        unsafe {
            let _ = SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
        }
    }
}

fn virtual_desktop() -> (i32, i32, i32, i32) {
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect() -> ScreenRect {
        ScreenRect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        }
    }

    #[test]
    fn alt_f4_is_filtered_but_f4_alone_is_not() {
        let mut injector = InputInjector::dry_run(rect());
        assert!(injector.key(VK_MENU, true));
        assert!(!injector.key(VK_F4, true));
        assert!(injector.key(VK_MENU, false));
        assert!(injector.key(VK_F4, true));
        assert!(injector.key(VK_F4, false));
    }

    #[test]
    fn release_all_clears_keys_and_buttons() {
        let mut injector = InputInjector::dry_run(rect());
        assert!(injector.key(0x41, true));
        injector.mouse_button(MouseButton::Left, true);
        injector.release_all();
        assert!(injector.held_keys.is_empty());
        assert!(injector.held_buttons.is_empty());
        assert!(
            injector
                .recorded
                .iter()
                .any(|event| event.contains("key 0x41 false"))
        );
        assert!(
            injector
                .recorded
                .iter()
                .any(|event| event.contains("button Left false"))
        );
    }

    #[test]
    fn normalized_points_map_inside_the_monitor() {
        let mut injector = InputInjector::dry_run(ScreenRect {
            x: 1920,
            y: 0,
            width: 1280,
            height: 720,
        });
        injector.mouse_move(0.0, 0.0);
        injector.mouse_move(1.0, 1.0);
        assert_eq!(injector.recorded[0], "move 1920,0");
        assert_eq!(injector.recorded[1], "move 3200,720");
    }
}
