//! M0/M3 texture gate: present a live 720p test pattern through FerroUI's Direct2D
//! `TexturePresenter` using the native D3D11 shared-frame path.
//!
//! `FERROUI_FPS` (default 10) and `FERROUI_SMOKE_EXIT_MS` drive the run. At exit the Direct2D
//! statistics show whether frames were imported on the GPU and whether any explicit CPU
//! readback happened.

#![cfg(windows)]

use ferroui_base::layout::{HorizontalAlignment, VerticalAlignment};
use ferroui_base::rendering::composition::frames::{
    DeviceIdentity, FrameAlpha, FrameBudget, FrameBudgetLimits, FrameColor, FrameFormat,
    FrameIdentity, FrameMailbox, FrameMetadata, FramePlane, FrameRegion, FrameRotation,
};
use ferroui_base::threading::{DispatcherPriority, DispatcherTimer};
use ferroui_base::{FerroObjectImpl, Ref, Thickness, ferro_class, ferro_impl_classes, instantiate};
use ferroui_controls::{
    AppBuilder, Application, ApplicationImpl, ApplicationImplExt, NewApplication, StackPanel,
    TextBlock, Window,
};
use ferroui_desktop::AppBuilderDesktopExtensions;
use ferroui_direct2d::{
    D3D11Texture, D3D11TextureImporter, device_generation, device_luid, native_device,
};
use ferroui_media::MediaSubscription;
use ferroui_media_controls::TexturePresenter;
use ferroui_themes_fluent::FluentTheme;
use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, ID3D11Device,
    ID3D11DeviceContext, ID3D11Resource, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::core::Interface;

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;
const STRIDE: u32 = WIDTH * 4;

thread_local! {
    static SOURCE: RefCell<Option<Rc<RefCell<FrameSource>>>> = const { RefCell::new(None) };
    static TIMER: RefCell<Option<Rc<DispatcherTimer>>> = const { RefCell::new(None) };
    static PRESENTED: Rc<Cell<u64>> = Rc::new(Cell::new(0));
    static PRESENTED_SUB: RefCell<Option<MediaSubscription>> = const { RefCell::new(None) };
}

/// Keeps the COM texture alive for the `FrameLease` resource. The resource is only
/// AddRef/Release from any thread; the pixels are written on the UI thread before publish.
struct TextureOwner(#[allow(dead_code)] ID3D11Texture2D);
unsafe impl Send for TextureOwner {}
unsafe impl Sync for TextureOwner {}

struct TextureEntry {
    texture: ID3D11Texture2D,
    view: D3D11Texture,
}

struct FrameSource {
    #[allow(dead_code)]
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    budget: FrameBudget,
    mailbox: Arc<FrameMailbox>,
    ring: Vec<TextureEntry>,
    generation: u64,
    sequence: u64,
    started: Instant,
    base: Vec<u8>,
    pixels: Vec<u8>,
}

fn create_texture(device: &ID3D11Device) -> Result<TextureEntry, String> {
    let description = D3D11_TEXTURE2D_DESC {
        Width: WIDTH,
        Height: HEIGHT,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut texture: Option<ID3D11Texture2D> = None;
    unsafe { device.CreateTexture2D(&description, None, Some(&mut texture)) }
        .map_err(|error| format!("CreateTexture2D: {error}"))?;
    let texture = texture.ok_or("CreateTexture2D returned no texture")?;
    let view =
        unsafe { D3D11Texture::from_texture(texture.as_raw(), 0, TextureOwner(texture.clone())) }
            .ok_or("D3D11Texture::from_texture returned null")?;
    Ok(TextureEntry { texture, view })
}

fn create_ring(device: &ID3D11Device) -> Result<Vec<TextureEntry>, String> {
    (0..TEXTURE_RING).map(|_| create_texture(device)).collect()
}

const TEXTURE_RING: usize = 8;

impl FrameSource {
    fn new() -> Result<Self, String> {
        let device = unsafe { ID3D11Device::from_raw(native_device()) };
        let context = unsafe { device.GetImmediateContext() }
            .map_err(|error| format!("the D3D11 device has no immediate context: {error}"))?;
        let generation = device_generation();
        let ring = create_ring(&device)?;
        let base = test_pattern_base();
        let pixels = base.clone();
        Ok(Self {
            device,
            context,
            budget: FrameBudget::new(FrameBudgetLimits {
                frames: 16,
                bytes: 512 * 1024 * 1024,
                max_dimension: 8192,
                gpu_reads: 16,
            }),
            mailbox: Arc::new(FrameMailbox::new(1, 1)),
            ring,
            generation,
            sequence: 0,
            started: Instant::now(),
            base,
            pixels,
        })
    }

    fn recreate_device(&mut self) -> Result<(), String> {
        let device = unsafe { ID3D11Device::from_raw(native_device()) };
        self.context = unsafe { device.GetImmediateContext() }.map_err(|error| {
            format!("the recreated D3D11 device has no immediate context: {error}")
        })?;
        self.ring = create_ring(&device)?;
        self.device = device;
        self.generation = device_generation();
        Ok(())
    }

    fn tick(&mut self) -> Result<u64, String> {
        if self.generation != device_generation() {
            self.recreate_device()?;
        }

        let sequence = self.sequence + 1;
        self.pixels.copy_from_slice(&self.base);
        decorate_pattern(&mut self.pixels, sequence);
        let pixels = &self.pixels;
        let index = (sequence as usize) % self.ring.len();
        let entry = &self.ring[index];

        let resource: ID3D11Resource = entry
            .texture
            .cast()
            .map_err(|error| format!("texture to resource: {error}"))?;
        unsafe {
            self.context.UpdateSubresource(
                &resource,
                0,
                None,
                pixels.as_ptr() as *const c_void,
                STRIDE,
                0,
            );
        }

        let metadata = FrameMetadata {
            identity: FrameIdentity {
                source: 1,
                generation: 1,
                sequence,
            },
            width: WIDTH,
            height: HEIGHT,
            visible: FrameRegion {
                x: 0,
                y: 0,
                width: WIDTH,
                height: HEIGHT,
            },
            format: FrameFormat::Bgra8,
            color: FrameColor::Srgb,
            alpha: FrameAlpha::Opaque,
            rotation: FrameRotation::None,
            mirrored: false,
            top_left_origin: true,
            timestamp: self.started.elapsed(),
            device: Some(DeviceIdentity::Luid(device_luid())),
            device_generation: self.generation,
            planes: vec![FramePlane {
                offset: 0,
                stride: STRIDE as u64,
                length: STRIDE as u64 * HEIGHT as u64,
            }],
            retained_bytes: STRIDE as u64 * HEIGHT as u64,
        };

        let lease = self
            .budget
            .lease(metadata, entry.view.clone())
            .map_err(|error| format!("frame budget: {error:?}"))?;
        self.mailbox
            .publish(lease)
            .map_err(|error| format!("mailbox publish: {error:?}"))?;
        self.sequence = sequence;
        Ok(sequence)
    }
}

/// BRGA static test pattern: eight colour bars.
fn test_pattern_base() -> Vec<u8> {
    const BARS: [[u8; 3]; 8] = [
        [0, 0, 0],
        [255, 255, 255],
        [255, 255, 0],
        [0, 255, 255],
        [0, 255, 0],
        [255, 0, 255],
        [255, 0, 0],
        [0, 0, 255],
    ];
    let mut pixels = vec![0u8; STRIDE as usize * HEIGHT as usize];
    for y in 0..HEIGHT {
        let row = &mut pixels[y as usize * STRIDE as usize..][..STRIDE as usize];
        for (x, pixel) in row.chunks_exact_mut(4).enumerate() {
            let bar = BARS[((x as u32 * BARS.len() as u32) / WIDTH) as usize];
            pixel[0] = bar[2];
            pixel[1] = bar[1];
            pixel[2] = bar[0];
            pixel[3] = 255;
        }
    }
    pixels
}

/// A moving block and a scan line on top of the static bars.
fn decorate_pattern(pixels: &mut [u8], sequence: u64) {
    let block_x = (sequence.wrapping_mul(24) % (WIDTH - 120) as u64) as u32;
    let block_y = ((sequence / 3) % (HEIGHT - 120) as u64) as u32;
    let scan_y = (sequence.wrapping_mul(13) % HEIGHT as u64) as u32;
    for y in block_y..block_y + 120 {
        let row = &mut pixels[y as usize * STRIDE as usize..][..STRIDE as usize];
        for x in block_x..block_x + 120 {
            row[x as usize * 4..x as usize * 4 + 4].copy_from_slice(&[0, 255, 255, 255]);
        }
    }
    let row = &mut pixels[scan_y as usize * STRIDE as usize..][..STRIDE as usize];
    for (x, pixel) in row.chunks_exact_mut(4).enumerate() {
        let value = (x % 255) as u8;
        pixel.copy_from_slice(&[value, value, value, 255]);
    }
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
        this.set_name(Some("FerroUI texture spike".to_string()));
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
    let source = Rc::new(RefCell::new(
        FrameSource::new().expect("the Direct2D device must be available"),
    ));
    let mailbox = source.borrow().mailbox.clone();
    SOURCE.with(|cell| *cell.borrow_mut() = Some(source.clone()));

    let header = TextBlock::new();
    header.set_text(Some("FerroUI Direct2D texture gate — 1280×720, GPU import"));
    header.set_font_size(18.0);

    let presenter = TexturePresenter::new();
    presenter.set_width(1024.0);
    presenter.set_height(576.0);
    presenter.set_horizontal_alignment(HorizontalAlignment::Center);
    presenter.set_vertical_alignment(VerticalAlignment::Center);
    presenter.set_frame_source(Some(mailbox), Some(Rc::new(D3D11TextureImporter)));

    let count = PRESENTED.with(|count| count.clone());
    let subscription = presenter.frame_presented().subscribe({
        let count = count.clone();
        move |_| count.set(count.get() + 1)
    });
    PRESENTED_SUB.with(|cell| *cell.borrow_mut() = Some(subscription));

    let panel = StackPanel::new();
    panel.set_spacing(12.0);
    panel.set_horizontal_alignment(HorizontalAlignment::Stretch);
    panel.set_vertical_alignment(VerticalAlignment::Stretch);
    panel.children().add(header);
    panel.children().add(presenter.clone());

    let window = Window::new();
    window.set_title(Some("FerroUI Direct2D texture gate".to_string()));
    window.set_width(1100.0);
    window.set_height(720.0);
    window.set_padding(Thickness::uniform(16.0));
    window.set_content(Some(ferroui_controls::Control::boxed(panel)));

    let fps: u64 = std::env::var("FERROUI_FPS")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(10);
    let interval = Duration::from_millis(1000 / fps);

    let presenter_handle = presenter.clone();
    let source_handle = source.clone();
    let timer = DispatcherTimer::with_callback(interval, DispatcherPriority::NORMAL, move |_| {
        let result = source_handle.borrow_mut().tick();
        match result {
            Ok(sequence) => {
                presenter_handle.present_latest();
                if sequence % 10 == 0 {
                    println!(
                        "frame {sequence} (fps {fps}), presented {}",
                        PRESENTED.with(|count| count.get())
                    );
                }
            }
            Err(error) => eprintln!("frame failed: {error}"),
        }
    });
    TIMER.with(|cell| *cell.borrow_mut() = Some(timer));

    window
        .opened(|| println!("window opened; {} fps", fps_or_default()))
        .detach();
    window.closed(|| {
        println!(
            "window closed; published {}; presented {}",
            SOURCE.with(|cell| cell.borrow().as_ref().map_or(0, |source| source.borrow().sequence)),
            PRESENTED.with(|count| count.get())
        );
        let stats = ferroui_direct2d::statistics();
        println!(
            "d2d stats: surfaces={} path_draws={} image_draws={} presents={} readbacks={} uploads={} texture_imports={} gpu_copies={} errors={} media_reads={} compute={} compute_px={}",
            stats[0],
            stats[1],
            stats[2],
            stats[3],
            stats[4],
            stats[5],
            stats[6],
            stats[7],
            stats[8],
            stats[9],
            stats[10],
            stats[11]
        );
    })
    .detach();

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

fn fps_or_default() -> u64 {
    std::env::var("FERROUI_FPS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(10)
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let exit_code = AppBuilder::configure::<App>()
        .use_platform_detect()
        .start_with_classic_desktop_lifetime(&args);
    std::process::ExitCode::from(exit_code as u8)
}
