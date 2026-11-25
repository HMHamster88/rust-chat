//! Monitor capture through Windows Graphics Capture.
//!
//! The capture thread is owned by Windows; frames are copied out as BGRA and sent to the
//! encoder task. Stop is cooperative: WGC only calls the handler when the screen changes,
//! so a static desktop can keep the thread alive until the next frame.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, HMONITOR, MONITORINFO};
use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};

#[derive(Debug, Clone)]
pub struct CapturedFrame {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
    pub timestamp_ms: u64,
}

struct CaptureFlags {
    frames: mpsc::UnboundedSender<CapturedFrame>,
    stop: Arc<AtomicBool>,
}

struct CaptureHandler {
    frames: mpsc::UnboundedSender<CapturedFrame>,
    stop: Arc<AtomicBool>,
    started: Instant,
    last: Instant,
}

impl GraphicsCaptureApiHandler for CaptureHandler {
    type Flags = CaptureFlags;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self {
            frames: ctx.flags.frames,
            stop: ctx.flags.stop,
            started: Instant::now(),
            last: Instant::now() - Duration::from_secs(1),
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        capture_control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        if self.stop.load(Ordering::Relaxed) {
            capture_control.stop();
            return Ok(());
        }
        // The capture API delivers on every change; the encoder only wants 10 fps.
        if self.last.elapsed() < Duration::from_millis(95) {
            return Ok(());
        }
        self.last = Instant::now();
        let width = frame.width();
        let height = frame.height();
        let buffer = frame.buffer()?;
        let mut storage = Vec::new();
        let pixels = buffer.as_nopadding_buffer(&mut storage);
        let _ = self.frames.send(CapturedFrame {
            width,
            height,
            bgra: pixels.to_vec(),
            timestamp_ms: self.started.elapsed().as_millis() as u64,
        });
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        self.stop.store(true, Ordering::Relaxed);
        Ok(())
    }
}

/// What the user chose to share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureSource {
    Monitor(usize),
    Window(String),
}

impl CaptureSource {
    pub fn id(&self) -> String {
        match self {
            Self::Monitor(index) => format!("monitor:{index}"),
            Self::Window(title) => format!("window:{title}"),
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        if let Some(index) = id.strip_prefix("monitor:") {
            return index.parse().ok().map(Self::Monitor);
        }
        id.strip_prefix("window:")
            .map(|title| Self::Window(title.to_owned()))
    }
}

#[derive(Debug, Clone)]
pub struct SourceInfo {
    pub source: CaptureSource,
    pub label: String,
}

pub fn enumerate_sources() -> Vec<SourceInfo> {
    let mut sources = Vec::new();
    if let Ok(monitors) = Monitor::enumerate() {
        for (index, monitor) in monitors.iter().enumerate() {
            let name = monitor.name().unwrap_or_default();
            sources.push(SourceInfo {
                source: CaptureSource::Monitor(index + 1),
                label: if name.is_empty() {
                    format!("Monitor {}", index + 1)
                } else {
                    format!("Monitor {} ({name})", index + 1)
                },
            });
        }
    }
    if let Ok(windows) = windows_capture::window::Window::enumerate() {
        for window in windows {
            let Ok(title) = window.title() else { continue };
            if title.trim().is_empty() || title == "Program Manager" {
                continue;
            }
            sources.push(SourceInfo {
                source: CaptureSource::Window(title.clone()),
                label: title,
            });
        }
    }
    sources
}

/// The screen rectangle a source occupies, used to map remote input.
pub fn capture_rect(source: &CaptureSource) -> Result<crate::input::ScreenRect, String> {
    match source {
        CaptureSource::Monitor(index) => monitor_rect(*index),
        CaptureSource::Window(title) => {
            let window = windows_capture::window::Window::from_contains_name(title)
                .map_err(|error| error.to_string())?;
            let rect = window.rect().map_err(|error| error.to_string())?;
            let width = rect.right - rect.left;
            let height = rect.bottom - rect.top;
            if width <= 0 || height <= 0 {
                return Err("the window has no visible area".to_owned());
            }
            Ok(crate::input::ScreenRect {
                x: rect.left,
                y: rect.top,
                width: width as u32,
                height: height as u32,
            })
        }
    }
}

pub struct ScreenCapture {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ScreenCapture {
    pub fn start(
        source: &CaptureSource,
        frames: mpsc::UnboundedSender<CapturedFrame>,
    ) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let source = source.clone();
        let source_for_thread = source.clone();
        let thread_stop = stop.clone();
        let thread = std::thread::Builder::new()
            .name("rust-chat-screen-capture".to_owned())
            .spawn(move || {
                let flags = CaptureFlags {
                    frames,
                    stop: thread_stop,
                };
                let result = match source_for_thread {
                    CaptureSource::Monitor(index) => match Monitor::from_index(index) {
                        Ok(monitor) => {
                            let settings = Settings::new(
                                monitor,
                                CursorCaptureSettings::Default,
                                DrawBorderSettings::Default,
                                SecondaryWindowSettings::Default,
                                MinimumUpdateIntervalSettings::Default,
                                DirtyRegionSettings::Default,
                                ColorFormat::Bgra8,
                                flags,
                            );
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                CaptureHandler::start(settings)
                            }))
                        }
                        Err(error) => {
                            eprintln!("screen capture: no monitor {index}: {error}");
                            return;
                        }
                    },
                    CaptureSource::Window(title) => {
                        match windows_capture::window::Window::from_contains_name(&title) {
                            Ok(window) => {
                                let settings = Settings::new(
                                    window,
                                    CursorCaptureSettings::Default,
                                    DrawBorderSettings::Default,
                                    SecondaryWindowSettings::Default,
                                    MinimumUpdateIntervalSettings::Default,
                                    DirtyRegionSettings::Default,
                                    ColorFormat::Bgra8,
                                    flags,
                                );
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    CaptureHandler::start(settings)
                                }))
                            }
                            Err(error) => {
                                eprintln!("screen capture: no window `{title}`: {error}");
                                return;
                            }
                        }
                    }
                };
                match result {
                    Ok(Ok(())) => eprintln!("screen capture: ended"),
                    Ok(Err(error)) => eprintln!("screen capture stopped: {error}"),
                    Err(payload) => {
                        let message = payload
                            .downcast_ref::<&str>()
                            .copied()
                            .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
                            .unwrap_or("unknown panic");
                        eprintln!("screen capture panicked: {message}");
                    }
                }
            })
            .map_err(|error| format!("cannot start the capture thread: {error}"))?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }

    pub fn start_monitor(
        index: usize,
        frames: mpsc::UnboundedSender<CapturedFrame>,
    ) -> Result<Self, String> {
        Self::start(&CaptureSource::Monitor(index), frames)
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // The WGC thread exits on the next frame; an idle desktop may delay it.
        self.thread = None;
    }
}

impl Drop for ScreenCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Position and size of a monitor in virtual-desktop pixels.
pub fn monitor_rect(index: usize) -> Result<crate::input::ScreenRect, String> {
    let monitor = Monitor::from_index(index).map_err(|error| error.to_string())?;
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe { GetMonitorInfoW(HMONITOR(monitor.as_raw_hmonitor()), &mut info) }
        .ok()
        .map_err(|error| error.to_string())?;
    let rect = info.rcMonitor;
    Ok(crate::input::ScreenRect {
        x: rect.left,
        y: rect.top,
        width: (rect.right - rect.left) as u32,
        height: (rect.bottom - rect.top) as u32,
    })
}
