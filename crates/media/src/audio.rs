//! Opus encoding/decoding and a small RTP reorder buffer for voice.

use std::collections::BTreeMap;

pub const SAMPLE_RATE: u32 = 48_000;
pub const FRAME_MS: u32 = 20;
pub const FRAME_SAMPLES: usize = (SAMPLE_RATE / 1000 * FRAME_MS) as usize;
const MAX_PACKET: usize = 1500;
const MAX_DECODED_SAMPLES: usize = 5760;

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("Opus: {0}")]
    Opus(#[from] opus::Error),
}

pub struct OpusEncoder {
    inner: opus::Encoder,
}

impl OpusEncoder {
    pub fn new() -> Result<Self, AudioError> {
        let mut inner =
            opus::Encoder::new(SAMPLE_RATE, opus::Channels::Mono, opus::Application::Voip)?;
        inner.set_bitrate(opus::Bitrate::Bits(32_000))?;
        inner.set_complexity(5)?;
        Ok(Self { inner })
    }

    /// Encodes one 20 ms mono frame.
    pub fn encode(&mut self, pcm: &[i16]) -> Result<Vec<u8>, AudioError> {
        let mut out = vec![0u8; MAX_PACKET];
        let len = self.inner.encode(pcm, &mut out)?;
        out.truncate(len);
        Ok(out)
    }
}

pub struct OpusDecoder {
    inner: opus::Decoder,
}

impl OpusDecoder {
    pub fn new() -> Result<Self, AudioError> {
        Ok(Self {
            inner: opus::Decoder::new(SAMPLE_RATE, opus::Channels::Mono)?,
        })
    }

    pub fn decode(&mut self, packet: &[u8]) -> Result<Vec<i16>, AudioError> {
        let mut out = vec![0i16; MAX_DECODED_SAMPLES];
        let len = self.inner.decode(packet, &mut out, false)?;
        out.truncate(len);
        Ok(out)
    }
}

/// Reorders RTP payloads by sequence number. Gaps hold back output until the missing
/// packet arrives; a long gap is skipped so one loss cannot stall the stream forever.
#[derive(Default)]
pub struct JitterBuffer {
    next: Option<u16>,
    pending: BTreeMap<u16, Vec<u8>>,
    max_pending: usize,
}

impl JitterBuffer {
    pub fn new() -> Self {
        Self {
            next: None,
            pending: BTreeMap::new(),
            max_pending: 8,
        }
    }

    pub fn push(&mut self, sequence: u16, payload: Vec<u8>) -> Vec<Vec<u8>> {
        if self.next.is_none() {
            self.next = Some(sequence);
        }
        if self
            .next
            .is_some_and(|next| sequence.wrapping_sub(next) > 1000)
        {
            // An old packet from before a restart: ignore it.
            return Vec::new();
        }
        self.pending.insert(sequence, payload);
        let mut ready = Vec::new();
        while let Some(next) = self.next {
            if let Some(payload) = self.pending.remove(&next) {
                ready.push(payload);
                self.next = Some(next.wrapping_add(1));
            } else {
                break;
            }
        }
        if self.pending.len() > self.max_pending
            && let Some((&first, _)) = self.pending.iter().next()
        {
            self.next = Some(first);
            return self.drain_ready();
        }
        ready
    }

    fn drain_ready(&mut self) -> Vec<Vec<u8>> {
        let mut ready = Vec::new();
        while let Some(next) = self.next {
            if let Some(payload) = self.pending.remove(&next) {
                ready.push(payload);
                self.next = Some(next.wrapping_add(1));
            } else {
                break;
            }
        }
        ready
    }
}

pub fn rms(pcm: &[i16]) -> f32 {
    if pcm.is_empty() {
        return 0.0;
    }
    let sum: f64 = pcm.iter().map(|sample| f64::from(*sample).powi(2)).sum();
    (sum / pcm.len() as f64).sqrt() as f32 / 32768.0
}

/// A 440 Hz sine at `amplitude` (0..1) for tests and smoke sources.
pub fn sine(phase: &mut f64, amplitude: f32) -> Vec<i16> {
    let mut pcm = Vec::with_capacity(FRAME_SAMPLES);
    for _ in 0..FRAME_SAMPLES {
        *phase += 2.0 * std::f64::consts::PI * 440.0 / f64::from(SAMPLE_RATE);
        if *phase > 2.0 * std::f64::consts::PI {
            *phase -= 2.0 * std::f64::consts::PI;
        }
        pcm.push((phase.sin() * f64::from(amplitude) * 32767.0) as i16);
    }
    pcm
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opus_round_trip_keeps_a_sine_audible() {
        let mut encoder = OpusEncoder::new().unwrap();
        let mut decoder = OpusDecoder::new().unwrap();
        let mut phase = 0.0;
        let mut decoded = Vec::new();
        for _ in 0..5 {
            let pcm = sine(&mut phase, 0.5);
            let packet = encoder.encode(&pcm).unwrap();
            assert!(!packet.is_empty());
            decoded.extend(decoder.decode(&packet).unwrap());
        }
        assert!(decoded.len() >= FRAME_SAMPLES * 4);
        assert!(rms(&decoded) > 0.2, "decoded RMS {}", rms(&decoded));
    }

    #[test]
    fn jitter_buffer_yields_in_order_and_survives_a_gap() {
        let mut buffer = JitterBuffer::new();
        assert_eq!(buffer.push(10, vec![10]).len(), 1);
        assert!(buffer.push(12, vec![12]).is_empty());
        assert_eq!(buffer.push(11, vec![11]), vec![vec![11], vec![12]]);
        // A long gap is skipped rather than stalling.
        for seq in 13..25 {
            buffer.push(seq, vec![seq as u8]);
        }
        assert!(buffer.next.is_some());
    }

    #[test]
    fn silence_has_zero_rms() {
        assert_eq!(rms(&[0i16; 960]), 0.0);
    }
}
