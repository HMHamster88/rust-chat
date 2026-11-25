//! VP8 screen codec: BGRA ↔ I420 conversion and thin wrappers over the pure-Rust
//! WebM VP8 encoder/decoder. No libvpx, no native toolchain.

use vp8_decoder::Decoder;
use vp8_encoder::{Encoder, EncoderConfig, Picture};

#[derive(Debug, thiserror::Error)]
pub enum VideoError {
    #[error("VP8 encoder: {0}")]
    Encode(String),
    #[error("VP8 decoder: {0}")]
    Decode(String),
    #[error("invalid frame size {width}x{height} for {len} bytes")]
    Size { width: u32, height: u32, len: usize },
}

pub struct EncodedVideo {
    pub data: Vec<u8>,
    pub keyframe: bool,
}

/// One VP8 encoder. Feed it I420 frames at the configured size.
pub struct VideoEncoder {
    inner: Encoder,
    width: u32,
    height: u32,
}

impl VideoEncoder {
    pub fn new(width: u32, height: u32, fps: u32, bitrate_bps: u32) -> Result<Self, VideoError> {
        let width = width.max(2) & !1;
        let height = height.max(2) & !1;
        let mut config = EncoderConfig::new(width, height, fps.max(1), bitrate_bps.max(64_000));
        config.max_bitrate_bps = config.max_bitrate_bps.max(config.bitrate_bps);
        config.keyframe_interval = Some(fps.max(1) * 5);
        let inner = Encoder::new(config).map_err(|error| VideoError::Encode(error.to_string()))?;
        Ok(Self {
            inner,
            width,
            height,
        })
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn request_keyframe(&mut self) {
        self.inner.request_keyframe();
    }

    pub fn set_rates(&mut self, bitrate_bps: u32, fps: u32) {
        self.inner.set_rates(bitrate_bps.max(64_000), fps.max(1));
    }

    /// Encodes one I420 frame. `None` means rate control skipped it.
    pub fn encode(
        &mut self,
        i420: &[u8],
        force_keyframe: bool,
    ) -> Result<Option<EncodedVideo>, VideoError> {
        let picture =
            Picture::from_i420(i420, self.width, self.height).ok_or(VideoError::Size {
                width: self.width,
                height: self.height,
                len: i420.len(),
            })?;
        let frame = self
            .inner
            .encode(&picture, force_keyframe)
            .map_err(|error| VideoError::Encode(error.to_string()))?;
        Ok(frame.map(|frame| EncodedVideo {
            data: frame.data.to_vec(),
            keyframe: frame.keyframe,
        }))
    }
}

#[derive(Clone)]
pub struct DecodedVideo {
    pub width: u32,
    pub height: u32,
    pub i420: Vec<u8>,
}

pub struct VideoDecoder {
    inner: Decoder,
}

impl Default for VideoDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl VideoDecoder {
    pub fn new() -> Self {
        Self {
            inner: Decoder::new(),
        }
    }

    pub fn reset(&mut self) {
        self.inner.reset();
    }

    pub fn dimensions(&self) -> Option<(u32, u32)> {
        self.inner.dimensions()
    }

    /// Decodes one complete VP8 frame. `Ok(None)` means the frame is not displayed.
    pub fn decode(&mut self, frame: &[u8]) -> Result<Option<DecodedVideo>, VideoError> {
        match self.inner.decode(frame) {
            Ok(Some(picture)) => Ok(Some(DecodedVideo {
                width: picture.width,
                height: picture.height,
                i420: picture.to_i420(),
            })),
            Ok(None) => Ok(None),
            Err(error) => Err(VideoError::Decode(format!("{error:?}"))),
        }
    }
}

/// BT.601 limited-range BGRA (sRGB) to tightly packed I420.
pub fn bgra_to_i420(bgra: &[u8], width: u32, height: u32) -> Vec<u8> {
    let width = width as usize;
    let height = height as usize;
    let chroma_width = width.div_ceil(2);
    let chroma_height = height.div_ceil(2);
    let mut out = vec![0u8; width * height + 2 * chroma_width * chroma_height];
    for y in 0..height {
        let row = &bgra[y * width * 4..][..width * 4];
        for x in 0..width {
            let pixel = &row[x * 4..x * 4 + 4];
            let b = f32::from(pixel[0]);
            let g = f32::from(pixel[1]);
            let r = f32::from(pixel[2]);
            out[y * width + x] =
                (16.0 + (65.738 * r + 129.057 * g + 25.064 * b) / 256.0).clamp(0.0, 255.0) as u8;
        }
    }
    let chroma_offset = width * height;
    for cy in 0..chroma_height {
        for cx in 0..chroma_width {
            let mut sum_u = 0.0f32;
            let mut sum_v = 0.0f32;
            let mut count = 0.0f32;
            for dy in 0..2 {
                for dx in 0..2 {
                    let x = (cx * 2 + dx).min(width - 1);
                    let y = (cy * 2 + dy).min(height - 1);
                    let pixel = &bgra[(y * width + x) * 4..][..4];
                    let b = f32::from(pixel[0]);
                    let g = f32::from(pixel[1]);
                    let r = f32::from(pixel[2]);
                    sum_u += (-37.945 * r - 74.494 * g + 112.439 * b) / 256.0 + 128.0;
                    sum_v += (112.439 * r - 94.154 * g - 18.285 * b) / 256.0 + 128.0;
                    count += 1.0;
                }
            }
            out[chroma_offset + cy * chroma_width + cx] = (sum_u / count).clamp(0.0, 255.0) as u8;
            out[chroma_offset + chroma_width * chroma_height + cy * chroma_width + cx] =
                (sum_v / count).clamp(0.0, 255.0) as u8;
        }
    }
    out
}

/// BT.601 limited-range I420 to opaque BGRA.
pub fn i420_to_bgra(i420: &[u8], width: u32, height: u32) -> Vec<u8> {
    let width_usize = width as usize;
    let height_usize = height as usize;
    let chroma_width = width_usize.div_ceil(2);
    let chroma_height = height_usize.div_ceil(2);
    let chroma_offset = width_usize * height_usize;
    let mut out = vec![0u8; width_usize * height_usize * 4];
    for y in 0..height_usize {
        for x in 0..width_usize {
            let luma = f32::from(i420[y * width_usize + x]);
            let u = f32::from(i420[chroma_offset + (y / 2) * chroma_width + (x / 2)]) - 128.0;
            let v = f32::from(
                i420[chroma_offset
                    + chroma_width * chroma_height
                    + (y / 2) * chroma_width
                    + (x / 2)],
            ) - 128.0;
            let c = (luma - 16.0).max(0.0) * 1.164;
            let r = c + 1.596 * v;
            let g = c - 0.391 * u - 0.813 * v;
            let b = c + 2.018 * u;
            let index = (y * width_usize + x) * 4;
            out[index] = b.clamp(0.0, 255.0) as u8;
            out[index + 1] = g.clamp(0.0, 255.0) as u8;
            out[index + 2] = r.clamp(0.0, 255.0) as u8;
            out[index + 3] = 255;
        }
    }
    out
}

/// Box-filter downscale of a BGRA image, preserving aspect within `max_width`/`max_height`.
pub fn fit_bgra(
    bgra: &[u8],
    width: u32,
    height: u32,
    max_width: u32,
    max_height: u32,
) -> (Vec<u8>, u32, u32) {
    if width <= max_width
        && height <= max_height
        && width.is_multiple_of(2)
        && height.is_multiple_of(2)
    {
        return (bgra.to_vec(), width, height);
    }
    let scale =
        (f64::from(max_width) / f64::from(width)).min(f64::from(max_height) / f64::from(height));
    let target_width = ((f64::from(width) * scale) as u32).max(2) & !1;
    let target_height = ((f64::from(height) * scale) as u32).max(2) & !1;
    let mut out = vec![0u8; target_width as usize * target_height as usize * 4];
    let x_ratio = width as f64 / f64::from(target_width);
    let y_ratio = height as f64 / f64::from(target_height);
    for ty in 0..target_height {
        let y0 = (f64::from(ty) * y_ratio) as u32;
        let y1 = (((f64::from(ty) + 1.0) * y_ratio) as u32)
            .min(height)
            .max(y0 + 1);
        for tx in 0..target_width {
            let x0 = (f64::from(tx) * x_ratio) as u32;
            let x1 = (((f64::from(tx) + 1.0) * x_ratio) as u32)
                .min(width)
                .max(x0 + 1);
            let mut sums = [0u32; 4];
            let mut count = 0u32;
            for y in y0..y1 {
                for x in x0..x1 {
                    let index = (y as usize * width as usize + x as usize) * 4;
                    for channel in 0..4 {
                        sums[channel] += u32::from(bgra[index + channel]);
                    }
                    count += 1;
                }
            }
            let index = (ty as usize * target_width as usize + tx as usize) * 4;
            let count = count.max(1);
            for channel in 0..4 {
                out[index + channel] = (sums[channel] / count) as u8;
            }
        }
    }
    (out, target_width, target_height)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(width: u32, height: u32) -> Vec<u8> {
        let mut bgra = vec![0u8; width as usize * height as usize * 4];
        for y in 0..height {
            for x in 0..width {
                let index = (y as usize * width as usize + x as usize) * 4;
                bgra[index] = (x * 255 / width.max(1)) as u8;
                bgra[index + 1] = (y * 255 / height.max(1)) as u8;
                bgra[index + 2] = (((x + y) * 255) / (width + height).max(1)) as u8;
                bgra[index + 3] = 255;
            }
        }
        bgra
    }

    #[test]
    fn color_round_trip_stays_close() {
        let width = 64;
        let height = 48;
        let bgra = pattern(width, height);
        let i420 = bgra_to_i420(&bgra, width, height);
        let back = i420_to_bgra(&i420, width, height);
        let mut worst = 0i16;
        for (a, b) in bgra
            .as_chunks::<4>()
            .0
            .iter()
            .zip(back.as_chunks::<4>().0.iter())
        {
            for channel in 0..3 {
                let delta = (i16::from(a[channel]) - i16::from(b[channel])).abs();
                worst = worst.max(delta);
            }
            assert_eq!(b[3], 255);
        }
        assert!(worst <= 10, "worst channel error {worst}");
    }

    #[test]
    fn encoder_round_trips_a_frame() {
        let width = 320;
        let height = 240;
        let mut encoder = VideoEncoder::new(width, height, 10, 500_000).unwrap();
        let mut decoder = VideoDecoder::new();
        let bgra = pattern(width, height);
        let i420 = bgra_to_i420(&bgra, width, height);
        let frame = encoder
            .encode(&i420, false)
            .unwrap()
            .expect("the first frame is a keyframe");
        assert!(frame.keyframe);
        let decoded = decoder.decode(&frame.data).unwrap().expect("a picture");
        assert_eq!((decoded.width, decoded.height), (width, height));
        let pixels = i420_to_bgra(&decoded.i420, decoded.width, decoded.height);
        assert!(pixels.iter().any(|sample| *sample != 0));
    }

    #[test]
    fn fit_downscales_to_720p_and_keeps_aspect() {
        let bgra = pattern(1920, 1080);
        let (scaled, width, height) = fit_bgra(&bgra, 1920, 1080, 1280, 720);
        assert_eq!((width, height), (1280, 720));
        assert_eq!(scaled.len(), 1280 * 720 * 4);
    }
}
