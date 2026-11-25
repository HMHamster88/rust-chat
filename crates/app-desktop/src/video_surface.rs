//! Direct2D presentation of decoded screen frames.
//!
//! This is the M3 texture path proven by `spikes/ferroui-texture`: BGRA is uploaded with
//! `UpdateSubresource`, published as a GPU lease and imported by FerroUI without readback.

use ferroui_base::Ref;
use ferroui_base::rendering::composition::frames::{
    DeviceIdentity, FrameAlpha, FrameBudget, FrameBudgetLimits, FrameColor, FrameFormat,
    FrameIdentity, FrameMailbox, FrameMetadata, FramePlane, FrameRegion, FrameRotation,
};
use ferroui_direct2d::{
    D3D11Texture, D3D11TextureImporter, device_generation, device_luid, native_device,
};
use ferroui_media::MediaSubscription;
use ferroui_media_controls::TexturePresenter;
use std::ffi::c_void;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, ID3D11Device,
    ID3D11DeviceContext, ID3D11Resource, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::core::Interface;

const TEXTURE_RING: usize = 6;
const MAX_WIDTH: u32 = 1280;
const MAX_HEIGHT: u32 = 720;

struct TextureOwner(#[allow(dead_code)] ID3D11Texture2D);
unsafe impl Send for TextureOwner {}
unsafe impl Sync for TextureOwner {}

struct TextureEntry {
    texture: ID3D11Texture2D,
    view: D3D11Texture,
}

pub struct VideoSurface {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    budget: FrameBudget,
    mailbox: Arc<FrameMailbox>,
    ring: Vec<TextureEntry>,
    generation: u64,
    sequence: u64,
    started: Instant,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl VideoSurface {
    pub fn new() -> Result<Self, String> {
        let device = unsafe { ID3D11Device::from_raw(native_device()) };
        let context = unsafe { device.GetImmediateContext() }
            .map_err(|error| format!("the D3D11 device has no immediate context: {error}"))?;
        let generation = device_generation();
        let width = MAX_WIDTH;
        let height = MAX_HEIGHT;
        let ring = create_ring(&device, width, height)?;
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
            width,
            height,
            pixels: vec![0u8; width as usize * height as usize * 4],
        })
    }

    pub fn mailbox(&self) -> Arc<FrameMailbox> {
        self.mailbox.clone()
    }

    fn recreate_device(&mut self) -> Result<(), String> {
        let device = unsafe { ID3D11Device::from_raw(native_device()) };
        self.context = unsafe { device.GetImmediateContext() }.map_err(|error| {
            format!("the recreated D3D11 device has no immediate context: {error}")
        })?;
        self.ring = create_ring(&device, self.width, self.height)?;
        self.device = device;
        self.generation = device_generation();
        Ok(())
    }

    /// Uploads one BGRA frame and publishes it to the presenter.
    pub fn publish(&mut self, width: u32, height: u32, bgra: &[u8]) -> Result<u64, String> {
        if width == 0 || height == 0 {
            return Err("empty video frame".to_owned());
        }
        if bgra.len() < width as usize * height as usize * 4 {
            return Err("short video frame".to_owned());
        }
        if (width, height) != (self.width, self.height) {
            let device = unsafe { ID3D11Device::from_raw(native_device()) };
            self.ring = create_ring(&device, width, height)?;
            self.device = device;
            self.generation = device_generation();
            self.width = width;
            self.height = height;
            self.pixels = vec![0u8; width as usize * height as usize * 4];
        }
        if self.generation != device_generation() {
            self.recreate_device()?;
        }

        let stride = width * 4;
        let expected = stride as usize * height as usize;
        if bgra.len() == expected {
            self.pixels.copy_from_slice(bgra);
        } else {
            for row in 0..height as usize {
                let source = row * stride as usize;
                let target = row * stride as usize;
                self.pixels[target..target + stride as usize]
                    .copy_from_slice(&bgra[source..source + stride as usize]);
            }
        }

        let sequence = self.sequence + 1;
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
                self.pixels.as_ptr() as *const c_void,
                stride,
                0,
            );
        }

        let metadata = FrameMetadata {
            identity: FrameIdentity {
                source: 1,
                generation: 1,
                sequence,
            },
            width,
            height,
            visible: FrameRegion {
                x: 0,
                y: 0,
                width,
                height,
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
                stride: u64::from(stride),
                length: u64::from(stride) * u64::from(height),
            }],
            retained_bytes: u64::from(stride) * u64::from(height),
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

fn create_texture(device: &ID3D11Device, width: u32, height: u32) -> Result<TextureEntry, String> {
    let description = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
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

fn create_ring(
    device: &ID3D11Device,
    width: u32,
    height: u32,
) -> Result<Vec<TextureEntry>, String> {
    (0..TEXTURE_RING)
        .map(|_| create_texture(device, width, height))
        .collect()
}

/// Creates the presenter and keeps its frame subscription alive.
pub fn build_presenter(mailbox: Arc<FrameMailbox>) -> (Ref<TexturePresenter>, MediaSubscription) {
    let presenter = TexturePresenter::new();
    presenter.set_frame_source(Some(mailbox), Some(Rc::new(D3D11TextureImporter)));
    let subscription = presenter.frame_presented().subscribe(|_| {});
    (presenter, subscription)
}
