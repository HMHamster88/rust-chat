//! Microphone capture and speaker playout with WebRTC echo cancellation.
//!
//! The audio callbacks stay on the device threads; the voice tasks only see 20 ms mono
//! 48 kHz frames and a lock-protected playout queue. APM processes 10 ms chunks on both
//! sides so the echo canceller can subtract what the speakers are playing.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Data, SampleFormat, SupportedStreamConfig};
use rust_chat_media::audio::{FRAME_SAMPLES, SAMPLE_RATE};
use rust_chat_media::{AudioDevice, AudioDevices};
use sonora::config::{Config, EchoCanceller, GainController2, NoiseSuppression};
use sonora::{AudioProcessing, StreamConfig};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// 10 ms at 48 kHz: the only frame size APM accepts.
const APM_SAMPLES: usize = (SAMPLE_RATE / 100) as usize;
/// 1 s of playout; longer means the sender is way ahead and we drop the oldest audio.
const PLAYOUT_CAP: usize = SAMPLE_RATE as usize;

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("no {0} device")]
    NoDevice(&'static str),
    #[error("audio device error: {0}")]
    Device(String),
    #[error("audio processing unavailable: {0}")]
    Processing(String),
    #[error("unsupported sample format {0:?}")]
    Format(SampleFormat),
}

#[derive(Debug, Clone)]
pub enum AudioEvent {
    InputLevel(f32),
    OutputLevel(f32),
    Error(String),
}

/// Controls shared with the device callbacks.
#[derive(Debug)]
pub struct AudioControl {
    mic_muted: AtomicBool,
    speaker_muted: AtomicBool,
    mic_gain: AtomicU32,
    speaker_gain: AtomicU32,
}

impl Default for AudioControl {
    fn default() -> Self {
        Self {
            mic_muted: AtomicBool::new(false),
            speaker_muted: AtomicBool::new(false),
            mic_gain: AtomicU32::new(1.0f32.to_bits()),
            speaker_gain: AtomicU32::new(1.0f32.to_bits()),
        }
    }
}

impl AudioControl {
    pub fn set_mic_muted(&self, muted: bool) {
        self.mic_muted.store(muted, Ordering::Relaxed);
    }

    pub fn mic_muted(&self) -> bool {
        self.mic_muted.load(Ordering::Relaxed)
    }

    pub fn set_speaker_muted(&self, muted: bool) {
        self.speaker_muted.store(muted, Ordering::Relaxed);
    }

    pub fn set_mic_gain(&self, gain: f32) {
        self.mic_gain
            .store(gain.clamp(0.0, 4.0).to_bits(), Ordering::Relaxed);
    }

    pub fn set_speaker_gain(&self, gain: f32) {
        self.speaker_gain
            .store(gain.clamp(0.0, 4.0).to_bits(), Ordering::Relaxed);
    }

    fn mic_gain(&self) -> f32 {
        f32::from_bits(self.mic_gain.load(Ordering::Relaxed))
    }

    fn speaker_gain(&self) -> f32 {
        f32::from_bits(self.speaker_gain.load(Ordering::Relaxed))
    }
}

/// Playback queue shared between the voice tasks and the output callback.
#[derive(Clone, Default)]
pub struct Playout(Arc<Mutex<VecDeque<f32>>>);

impl Playout {
    pub fn push(&self, pcm: &[i16]) {
        if let Ok(mut queue) = self.0.lock() {
            for sample in pcm {
                queue.push_back(f32::from(*sample) / 32768.0);
            }
            while queue.len() > PLAYOUT_CAP {
                queue.pop_front();
            }
        }
    }

    /// Number of queued 48 kHz samples (tests and diagnostics).
    pub fn len(&self) -> usize {
        self.0.lock().map(|queue| queue.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Removes everything queued so far. The device callback uses `next` instead.
    pub fn drain(&self) -> Vec<f32> {
        self.0
            .lock()
            .map(|mut queue| queue.drain(..).collect())
            .unwrap_or_default()
    }

    fn next(&self) -> f32 {
        self.0
            .lock()
            .ok()
            .and_then(|mut queue| queue.pop_front())
            .unwrap_or(0.0)
    }

    fn clear(&self) {
        if let Ok(mut queue) = self.0.lock() {
            queue.clear();
        }
    }
}

/// The running streams. Keep this on the thread that created it: some backends require it.
pub struct AudioIo {
    input: Option<cpal::Stream>,
    output: Option<cpal::Stream>,
    control: Arc<AudioControl>,
    playout: Playout,
}

/// Everything the voice tasks need from the audio layer.
pub struct AudioHandle {
    pub frames: mpsc::UnboundedReceiver<Vec<i16>>,
    pub events: mpsc::UnboundedReceiver<AudioEvent>,
    pub playout: Playout,
    pub control: Arc<AudioControl>,
}

impl AudioIo {
    pub fn start(
        input_id: Option<&str>,
        output_id: Option<&str>,
    ) -> Result<(Self, AudioHandle), AudioError> {
        let host = cpal::default_host();
        let input_device = pick_device(&host, input_id, true)?;
        let output_device = pick_device(&host, output_id, false)?;
        let input_support = pick_config(&input_device, true)?;
        let output_support = pick_config(&output_device, false)?;
        let input_rate = input_support.sample_rate();
        let output_rate = output_support.sample_rate();

        let processor = Arc::new(Mutex::new(
            AudioProcessing::builder()
                .config(apm_config())
                .capture_config(StreamConfig::new(SAMPLE_RATE, 1))
                .render_config(StreamConfig::new(SAMPLE_RATE, 1))
                .build(),
        ));

        let (frame_tx, frame_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let control = Arc::new(AudioControl::default());
        let playout = Playout::default();

        let mut capture_resampler = Resampler::new(input_rate, SAMPLE_RATE);
        let mut capture_samples = Vec::with_capacity(APM_SAMPLES * 2);
        let mut pending_pcm = Vec::with_capacity(FRAME_SAMPLES * 2);
        let input_format = input_support.sample_format();
        let input_channels = input_support.channels() as usize;
        let input_config = input_support.config();
        let callback_control = control.clone();
        let callback_events = event_tx.clone();
        let capture_processor = processor.clone();
        let input_stream = input_device
            .build_input_stream_raw(
                input_config,
                input_format,
                move |data, _| {
                    let mono = match downmix(data, input_format, input_channels) {
                        Ok(mono) => mono,
                        Err(error) => {
                            let _ = callback_events.send(AudioEvent::Error(error.to_string()));
                            return;
                        }
                    };
                    let resampled = capture_resampler.push(&mono);
                    capture_samples.extend_from_slice(&resampled);
                    while capture_samples.len() >= APM_SAMPLES {
                        let mut chunk: Vec<f32> = capture_samples.drain(..APM_SAMPLES).collect();
                        let input = chunk.clone();
                        let processed = capture_processor.lock().ok().and_then(|mut apm| {
                            apm.process_capture_f32(
                                &[input.as_slice()],
                                &mut [chunk.as_mut_slice()],
                            )
                            .ok()
                        });
                        if processed.is_none() {
                            continue;
                        }
                        let gain = callback_control.mic_gain();
                        for sample in chunk {
                            pending_pcm.push(sample * gain);
                        }
                        if pending_pcm.len() >= FRAME_SAMPLES {
                            let level = rms_f32(&pending_pcm[..FRAME_SAMPLES]);
                            let pcm: Vec<i16> = pending_pcm
                                .drain(..FRAME_SAMPLES)
                                .map(|sample| (sample.clamp(-1.0, 1.0) * 32767.0) as i16)
                                .collect();
                            let _ = frame_tx.send(pcm);
                            let _ = callback_events.send(AudioEvent::InputLevel(level));
                        }
                    }
                },
                {
                    let events = event_tx.clone();
                    move |error| {
                        if fatal_audio_error(&error) {
                            let _ = events.send(AudioEvent::Error(format!("microphone: {error}")));
                        }
                    }
                },
                None,
            )
            .map_err(|error| AudioError::Device(error.to_string()))?;

        let output_format = output_support.sample_format();
        let output_channels = output_support.channels() as usize;
        let output_config = output_support.config();
        let mut render_resampler = Resampler::new(SAMPLE_RATE, output_rate);
        let callback_playout = playout.clone();
        let callback_control = control.clone();
        let callback_events = event_tx.clone();
        let output_stream = output_device
            .build_output_stream_raw(
                output_config,
                output_format,
                move |data, _| {
                    let frames = data.len() / output_channels;
                    if frames == 0 {
                        return;
                    }
                    let source_frames = ((frames as f64) * (SAMPLE_RATE as f64)
                        / (output_rate as f64))
                        .ceil() as usize
                        + 2;
                    let mut source = Vec::with_capacity(source_frames);
                    for _ in 0..source_frames {
                        source.push(callback_playout.next());
                    }
                    let gain = callback_control.speaker_gain();
                    let muted = callback_control.speaker_muted.load(Ordering::Relaxed);
                    for sample in &mut source {
                        if muted {
                            *sample = 0.0;
                        } else {
                            *sample = (*sample * gain).clamp(-1.0, 1.0);
                        }
                    }
                    // Let the echo canceller know exactly what is leaving the speakers.
                    let mut rendered = Vec::with_capacity(source.len());
                    if let Ok(mut apm) = processor.lock() {
                        for chunk in source.chunks(APM_SAMPLES) {
                            let mut output = vec![0.0f32; chunk.len()];
                            if apm
                                .process_render_f32(&[chunk], &mut [output.as_mut_slice()])
                                .is_ok()
                            {
                                rendered.extend_from_slice(&output);
                            } else {
                                rendered.extend_from_slice(chunk);
                            }
                        }
                    } else {
                        rendered.extend_from_slice(&source);
                    }
                    let _ = callback_events.send(AudioEvent::OutputLevel(rms_f32(&rendered)));

                    let mut out = render_resampler.push(&rendered);
                    if out.len() < frames {
                        out.resize(frames, 0.0);
                    }
                    write_output(data, output_format, output_channels, &out[..frames]);
                },
                {
                    let events = event_tx.clone();
                    move |error| {
                        if fatal_audio_error(&error) {
                            let _ = events.send(AudioEvent::Error(format!("speakers: {error}")));
                        }
                    }
                },
                None,
            )
            .map_err(|error| AudioError::Device(error.to_string()))?;

        input_stream
            .play()
            .map_err(|error| AudioError::Device(error.to_string()))?;
        output_stream
            .play()
            .map_err(|error| AudioError::Device(error.to_string()))?;

        Ok((
            Self {
                input: Some(input_stream),
                output: Some(output_stream),
                control: control.clone(),
                playout: playout.clone(),
            },
            AudioHandle {
                frames: frame_rx,
                events: event_rx,
                playout,
                control,
            },
        ))
    }

    pub fn control(&self) -> Arc<AudioControl> {
        self.control.clone()
    }

    pub fn playout(&self) -> Playout {
        self.playout.clone()
    }

    /// Stops capturing and clears the playout queue.
    pub fn stop(&mut self) {
        if let Some(stream) = self.input.take() {
            let _ = stream.pause();
        }
        if let Some(stream) = self.output.take() {
            let _ = stream.pause();
        }
        self.playout.clear();
    }
}

fn apm_config() -> Config {
    Config {
        echo_canceller: Some(EchoCanceller::default()),
        noise_suppression: Some(NoiseSuppression::default()),
        gain_controller2: Some(GainController2::default()),
        ..Default::default()
    }
}

/// Lists input and output devices in the same order `pick_device` uses the ids.
pub fn audio_devices() -> AudioDevices {
    let host = cpal::default_host();
    let mut devices = AudioDevices::default();
    let default_input = host
        .default_input_device()
        .map(|device| device.to_string())
        .unwrap_or_default();
    if let Ok(list) = host.input_devices() {
        for (index, device) in list.enumerate() {
            let name = device.to_string();
            devices.inputs.push(AudioDevice {
                id: format!("input:{index}"),
                is_default: name == default_input,
                name,
            });
        }
    }
    let default_output = host
        .default_output_device()
        .map(|device| device.to_string())
        .unwrap_or_default();
    if let Ok(list) = host.output_devices() {
        for (index, device) in list.enumerate() {
            let name = device.to_string();
            devices.outputs.push(AudioDevice {
                id: format!("output:{index}"),
                is_default: name == default_output,
                name,
            });
        }
    }
    devices
}

fn device_index(id: Option<&str>) -> Option<usize> {
    id.and_then(|id| id.rsplit(':').next())
        .and_then(|index| index.parse().ok())
}

fn pick_device(
    host: &cpal::Host,
    id: Option<&str>,
    input: bool,
) -> Result<cpal::Device, AudioError> {
    let index = device_index(id);
    if input {
        let list: Vec<_> = host
            .input_devices()
            .map_err(|error| AudioError::Device(error.to_string()))?
            .collect();
        if let Some(index) = index
            && let Some(device) = list.get(index)
        {
            return Ok(device.clone());
        }
        host.default_input_device()
            .ok_or(AudioError::NoDevice("microphone"))
    } else {
        let list: Vec<_> = host
            .output_devices()
            .map_err(|error| AudioError::Device(error.to_string()))?
            .collect();
        if let Some(index) = index
            && let Some(device) = list.get(index)
        {
            return Ok(device.clone());
        }
        host.default_output_device()
            .ok_or(AudioError::NoDevice("speaker"))
    }
}

fn pick_config(device: &cpal::Device, input: bool) -> Result<SupportedStreamConfig, AudioError> {
    let supported = if input {
        device.supported_input_configs().ok().and_then(pick_48k)
    } else {
        device.supported_output_configs().ok().and_then(pick_48k)
    };
    if let Some(config) = supported {
        return Ok(config);
    }
    let config = if input {
        device.default_input_config()
    } else {
        device.default_output_config()
    }
    .map_err(|error| AudioError::Device(error.to_string()))?;
    if matches!(
        config.sample_format(),
        SampleFormat::F32 | SampleFormat::I16 | SampleFormat::U16
    ) {
        Ok(config)
    } else {
        Err(AudioError::Format(config.sample_format()))
    }
}

fn pick_48k(
    ranges: impl Iterator<Item = cpal::SupportedStreamConfigRange>,
) -> Option<SupportedStreamConfig> {
    for range in ranges {
        if range.min_sample_rate() <= SAMPLE_RATE
            && range.max_sample_rate() >= SAMPLE_RATE
            && matches!(
                range.sample_format(),
                SampleFormat::F32 | SampleFormat::I16 | SampleFormat::U16
            )
        {
            return Some(range.with_sample_rate(SAMPLE_RATE));
        }
    }
    None
}

fn downmix(data: &Data, format: SampleFormat, channels: usize) -> Result<Vec<f32>, AudioError> {
    let channels = channels.max(1);
    let mono = match format {
        SampleFormat::F32 => {
            let Some(samples) = data.as_slice::<f32>() else {
                return Err(AudioError::Format(format));
            };
            samples
                .chunks_exact(channels)
                .map(|frame| frame.iter().sum::<f32>() / channels as f32)
                .collect()
        }
        SampleFormat::I16 => {
            let Some(samples) = data.as_slice::<i16>() else {
                return Err(AudioError::Format(format));
            };
            samples
                .chunks_exact(channels)
                .map(|frame| {
                    frame
                        .iter()
                        .map(|sample| f32::from(*sample) / 32768.0)
                        .sum::<f32>()
                        / channels as f32
                })
                .collect()
        }
        SampleFormat::U16 => {
            let Some(samples) = data.as_slice::<u16>() else {
                return Err(AudioError::Format(format));
            };
            samples
                .chunks_exact(channels)
                .map(|frame| {
                    frame
                        .iter()
                        .map(|sample| (f32::from(*sample) - 32768.0) / 32768.0)
                        .sum::<f32>()
                        / channels as f32
                })
                .collect()
        }
        other => return Err(AudioError::Format(other)),
    };
    Ok(mono)
}

fn write_output(data: &mut Data, format: SampleFormat, channels: usize, samples: &[f32]) {
    let channels = channels.max(1);
    match format {
        SampleFormat::F32 => {
            let Some(out) = data.as_slice_mut::<f32>() else {
                return;
            };
            for (index, frame) in out.chunks_exact_mut(channels).enumerate() {
                let value = samples.get(index).copied().unwrap_or(0.0);
                frame.fill(value);
            }
        }
        SampleFormat::I16 => {
            let Some(out) = data.as_slice_mut::<i16>() else {
                return;
            };
            for (index, frame) in out.chunks_exact_mut(channels).enumerate() {
                let value = samples.get(index).copied().unwrap_or(0.0);
                frame.fill((value.clamp(-1.0, 1.0) * 32767.0) as i16);
            }
        }
        SampleFormat::U16 => {
            let Some(out) = data.as_slice_mut::<u16>() else {
                return;
            };
            for (index, frame) in out.chunks_exact_mut(channels).enumerate() {
                let value = samples.get(index).copied().unwrap_or(0.0);
                frame.fill(((value.clamp(-1.0, 1.0) * 32767.0) + 32768.0) as u16);
            }
        }
        _ => {}
    }
}

/// Xruns and automatic rerouting do not end a call; everything else is fatal.
fn fatal_audio_error(error: &cpal::Error) -> bool {
    !matches!(
        error.kind(),
        cpal::ErrorKind::Xrun | cpal::ErrorKind::DeviceChanged
    )
}

fn rms_f32(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32).sqrt()
}

/// Linear mono resampler with persistent fractional position.
struct Resampler {
    step: f64,
    position: f64,
    previous: f32,
    started: bool,
}

impl Resampler {
    fn new(from: u32, to: u32) -> Self {
        Self {
            step: from as f64 / to as f64,
            position: 0.0,
            previous: 0.0,
            started: false,
        }
    }

    /// Resamples one device callback worth of mono samples. When rates match this is a copy.
    fn push(&mut self, source: &[f32]) -> Vec<f32> {
        if (self.step - 1.0).abs() < f64::EPSILON {
            return source.to_vec();
        }
        if source.is_empty() {
            return Vec::new();
        }
        if !self.started {
            self.previous = source[0];
            self.started = true;
        }
        let mut out = Vec::with_capacity(((source.len() as f64) / self.step).ceil() as usize + 2);
        let total = source.len() as f64 + 1.0;
        while self.position + 1.0 < total {
            let index = self.position.floor() as usize;
            let fraction = (self.position - index as f64) as f32;
            let a = if index == 0 {
                self.previous
            } else {
                source[index - 1]
            };
            let b = source[index];
            out.push(a + (b - a) * fraction);
            self.position += self.step;
        }
        self.position -= source.len() as f64;
        self.previous = *source.last().unwrap_or(&self.previous);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampler_preserves_length_and_extremes() {
        let mut up = Resampler::new(24_000, 48_000);
        let out = up.push(&[0.0, 0.0, 0.0, 0.0]);
        assert!(out.len() >= 6);
        assert!(out.iter().all(|sample| sample.abs() < 0.001));

        let mut down = Resampler::new(48_000, 24_000);
        let dc: Vec<f32> = vec![0.5; 480];
        let out = down.push(&dc);
        assert!(out.len() >= 238 && out.len() <= 242, "len {}", out.len());
        assert!(out.iter().all(|sample| (*sample - 0.5).abs() < 0.001));
    }

    #[test]
    fn playout_drops_old_audio_instead_of_growing() {
        let playout = Playout::default();
        for _ in 0..200 {
            playout.push(&vec![1000i16; FRAME_SAMPLES]);
        }
        let len = playout.0.lock().unwrap().len();
        assert!(len <= PLAYOUT_CAP);
        assert!(len >= PLAYOUT_CAP - FRAME_SAMPLES);
    }

    #[test]
    fn rms_of_half_scale_sine_is_reasonable() {
        let pcm: Vec<i16> = (0..480)
            .map(|i| ((i as f32 / 480.0 * std::f32::consts::TAU).sin() * 16000.0) as i16)
            .collect();
        let level = rms_f32(
            &pcm.iter()
                .map(|s| f32::from(*s) / 32768.0)
                .collect::<Vec<_>>(),
        );
        assert!(level > 0.2 && level < 0.4, "level {level}");
    }
}
