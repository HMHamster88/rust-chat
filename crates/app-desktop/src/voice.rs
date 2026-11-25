//! Per-peer voice mesh on top of the WebSocket signal relay.
//!
//! The room is the mesh: every member with `voice = true` gets one WebRTC peer
//! connection with an Opus track. To avoid offer glare, the member with the smaller
//! session id creates the offer; the other side answers. SDP carries its host
//! candidates inline, so no trickle ICE is needed.

use rust_chat_client_core::{ClientHandle, IncomingSignal};
use rust_chat_media::audio::{FRAME_SAMPLES, rms};
use rust_chat_media::video::{VideoEncoder, bgra_to_i420, fit_bgra, i420_to_bgra};
use rust_chat_media::voice::{VoicePeer, turn_ice_servers};
use rust_chat_media_desktop::audio::{AudioEvent, AudioHandle, Playout};
use rust_chat_media_desktop::capture::{CapturedFrame, ScreenCapture};
use rust_chat_protocol::payloads::TurnCredentials;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(25);
const LEVEL_INTERVAL: Duration = Duration::from_millis(150);
const SHARE_FPS: u32 = 10;
const SHARE_INTERVAL: Duration = Duration::from_millis(1000 / SHARE_FPS as u64);
const SHARE_BITRATE: u32 = 1_500_000;
const SHARE_MAX_WIDTH: u32 = 1280;
const SHARE_MAX_HEIGHT: u32 = 720;

#[derive(Clone)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
    pub at: Instant,
}

/// The latest remote screen frame, taken by the UI at its own pace.
#[derive(Clone, Default)]
pub struct VideoSink(Arc<Mutex<Option<VideoFrame>>>);

impl VideoSink {
    pub fn publish(&self, frame: VideoFrame) {
        if let Ok(mut slot) = self.0.lock() {
            *slot = Some(frame);
        }
    }

    pub fn take(&self) -> Option<VideoFrame> {
        self.0.lock().ok().and_then(|mut slot| slot.take())
    }

    pub fn clear(&self) {
        if let Ok(mut slot) = self.0.lock() {
            *slot = None;
        }
    }
}

#[derive(Debug)]
pub enum VoiceCommand {
    /// The current set of members that announce `voice = true` (including self).
    Members(Vec<String>),
    MicMuted(bool),
    SpeakerMuted(bool),
    Share(bool),
    Stop,
}

#[derive(Debug)]
pub enum VoiceEvent {
    Ready(String),
    Level {
        peer: String,
        level: f32,
    },
    SelfLevel(f32),
    Stats {
        peer: String,
        rtt_ms: Option<u32>,
        relayed: bool,
    },
    PeerFailed(String),
    Ended,
    Error(String),
}

pub struct VoiceController {
    commands: mpsc::UnboundedSender<VoiceCommand>,
    events: mpsc::UnboundedReceiver<VoiceEvent>,
    video: VideoSink,
}

impl VoiceController {
    pub fn send(&self, command: VoiceCommand) {
        let _ = self.commands.send(command);
    }

    pub fn try_recv(&mut self) -> Option<VoiceEvent> {
        self.events.try_recv().ok()
    }

    /// Remote screen frames decoded by the peer tasks.
    pub fn video(&self) -> VideoSink {
        self.video.clone()
    }
}

/// Starts the controller. The returned value keeps the command channel open; dropping it
/// ends the mesh.
pub fn start(
    client: ClientHandle,
    self_id: String,
    handle: AudioHandle,
    runtime: &tokio::runtime::Handle,
    turn: Option<TurnCredentials>,
) -> VoiceController {
    let signals = client.subscribe_signals();
    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let (local_tx, _) = broadcast::channel::<Vec<i16>>(16);
    let (video_tx, _) = broadcast::channel::<Arc<Vec<u8>>>(4);
    let (share_tx, share_rx) = mpsc::unbounded_channel();
    let (capture_tx, capture_rx) = mpsc::unbounded_channel::<CapturedFrame>();
    let (down_tx, down_rx) = mpsc::unbounded_channel();
    let (up_tx, up_rx) = mpsc::unbounded_channel();
    let (wake_tx, wake_rx) = mpsc::unbounded_channel();
    let force_keyframe = Arc::new(AtomicBool::new(false));
    let video = VideoSink::default();
    runtime.spawn(share_task(
        share_rx,
        capture_tx,
        capture_rx,
        video_tx.clone(),
        event_tx.clone(),
        force_keyframe.clone(),
    ));
    let run = VoiceRun {
        client,
        self_id,
        handle,
        signals,
        commands: command_rx,
        events: event_tx,
        peers: HashMap::new(),
        members: HashSet::new(),
        retries: HashMap::new(),
        down: down_tx,
        down_rx,
        up: up_tx,
        up_rx,
        wake: wake_tx,
        wake_rx,
        force_keyframe,
        turn,
        local: local_tx,
        video: video_tx,
        share: share_tx,
        sink: video.clone(),
        mic_muted: true,
        speaker_muted: false,
        last_self_level: Instant::now() - LEVEL_INTERVAL,
    };
    runtime.spawn(run.run());
    VoiceController {
        commands: command_tx,
        events: event_rx,
        video,
    }
}

struct PeerHandle {
    signals: mpsc::UnboundedSender<PeerSignal>,
    task: JoinHandle<()>,
}

enum PeerSignal {
    Answer(String),
}

struct VoiceRun {
    client: ClientHandle,
    self_id: String,
    handle: AudioHandle,
    signals: broadcast::Receiver<IncomingSignal>,
    commands: mpsc::UnboundedReceiver<VoiceCommand>,
    events: mpsc::UnboundedSender<VoiceEvent>,
    peers: HashMap<String, PeerHandle>,
    members: HashSet<String>,
    retries: HashMap<String, u32>,
    down: mpsc::UnboundedSender<(String, Option<String>)>,
    down_rx: mpsc::UnboundedReceiver<(String, Option<String>)>,
    up: mpsc::UnboundedSender<String>,
    up_rx: mpsc::UnboundedReceiver<String>,
    wake: mpsc::UnboundedSender<String>,
    wake_rx: mpsc::UnboundedReceiver<String>,
    force_keyframe: Arc<AtomicBool>,
    turn: Option<TurnCredentials>,
    local: broadcast::Sender<Vec<i16>>,
    video: broadcast::Sender<Arc<Vec<u8>>>,
    share: mpsc::UnboundedSender<bool>,
    sink: VideoSink,
    mic_muted: bool,
    speaker_muted: bool,
    last_self_level: Instant,
}

impl VoiceRun {
    async fn run(mut self) {
        self.presence();
        loop {
            tokio::select! {
                command = self.commands.recv() => match command {
                    Some(VoiceCommand::Members(members)) => self.sync_members(members),
                    Some(VoiceCommand::MicMuted(muted)) => {
                        self.mic_muted = muted;
                        self.handle.control.set_mic_muted(muted);
                        self.presence();
                    }
                    Some(VoiceCommand::SpeakerMuted(muted)) => {
                        self.speaker_muted = muted;
                        self.handle.control.set_speaker_muted(muted);
                        self.presence();
                    }
                    Some(VoiceCommand::Share(on)) => {
                        let _ = self.share.send(on);
                        self.client.set_presence(None, None, Some(on));
                        if !on {
                            self.sink.clear();
                        }
                    }
                    Some(VoiceCommand::Stop) | None => break,
                },
                signal = self.signals.recv() => match signal {
                    Ok(signal) => self.route_signal(signal),
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                down = self.down_rx.recv() => {
                    if let Some((id, error)) = down {
                        self.peer_down(id, error);
                    }
                }
                up = self.up_rx.recv() => {
                    if let Some(id) = up {
                        self.retries.remove(&id);
                    }
                }
                wake = self.wake_rx.recv() => {
                    if let Some(id) = wake {
                        self.retry_peer(id);
                    }
                }
                frame = self.handle.frames.recv() => {
                    let Some(frame) = frame else {
                        let _ = self.events.send(VoiceEvent::Error(
                            "the microphone stream stopped".to_owned(),
                        ));
                        break;
                    };
                    if frame.len() == FRAME_SAMPLES && !self.mic_muted {
                        let _ = self.local.send(frame);
                    }
                },
                event = self.handle.events.recv() => match event {
                    Some(AudioEvent::InputLevel(level)) => {
                        if self.last_self_level.elapsed() >= LEVEL_INTERVAL {
                            self.last_self_level = Instant::now();
                            let _ = self.events.send(VoiceEvent::SelfLevel(if self.mic_muted {
                                0.0
                            } else {
                                level
                            }));
                        }
                    }
                    Some(AudioEvent::Error(error)) => {
                        let _ = self.events.send(VoiceEvent::Error(error));
                        break;
                    }
                    _ => {}
                },
            }
        }
        self.shutdown();
    }

    fn presence(&self) {
        self.client
            .set_presence(Some(self.mic_muted), Some(self.speaker_muted), None);
    }

    fn sync_members(&mut self, members: Vec<String>) {
        self.members = members.iter().cloned().collect();
        let wanted: std::collections::HashSet<&String> = members.iter().collect();
        let stale: Vec<String> = self
            .peers
            .keys()
            .filter(|id| !wanted.contains(id))
            .cloned()
            .collect();
        for id in stale {
            if let Some(peer) = self.peers.remove(&id) {
                peer.task.abort();
            }
        }
        self.retries.retain(|id, _| self.members.contains(id));
        for id in members {
            if id == self.self_id || self.peers.contains_key(&id) {
                continue;
            }
            if self.self_id < id {
                self.add_offerer(id);
            }
            // The larger id waits for the other side's offer.
        }
    }

    fn peer_down(&mut self, id: String, error: Option<String>) {
        if self.peers.remove(&id).is_none() {
            return;
        }
        if !self.members.contains(&id) {
            self.retries.remove(&id);
            return;
        }
        if let Some(error) = error {
            let _ = self.events.send(VoiceEvent::PeerFailed(error));
        }
        // Only the designated offerer rebuilds the connection; the other side waits.
        if self.self_id >= id {
            return;
        }
        let attempt = self.retries.entry(id.clone()).or_insert(0);
        *attempt += 1;
        if *attempt > 3 {
            let _ = self.events.send(VoiceEvent::PeerFailed(format!(
                "cannot reach peer {id} after 3 attempts"
            )));
            return;
        }
        let delay = Duration::from_millis(500u64 << (*attempt - 1).min(4));
        let wake = self.wake.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let _ = wake.send(id);
        });
    }

    fn retry_peer(&mut self, id: String) {
        if !self.members.contains(&id) || self.peers.contains_key(&id) || self.self_id >= id {
            return;
        }
        self.add_offerer(id);
    }

    fn add_offerer(&mut self, id: String) {
        let (signal_tx, signal_rx) = mpsc::unbounded_channel();
        let client = self.client.clone();
        let local = self.local.subscribe();
        let video = self.video.subscribe();
        let sink = self.sink.clone();
        let events = self.events.clone();
        let playout = self.handle.playout.clone();
        let down = self.down.clone();
        let up = self.up.clone();
        let turn = self.turn.clone();
        let peer_id = id.clone();
        let task = tokio::spawn(async move {
            let result = run_peer(
                peer_id.clone(),
                Offer::Create,
                client.clone(),
                local,
                video,
                signal_rx,
                playout,
                sink,
                events,
                turn,
                up,
            )
            .await;
            if result.is_err() {
                let _ = client.send_signal(&peer_id, json!({ "kind": "bye" }));
            }
            let _ = down.send((peer_id, result.err()));
        });
        self.peers.insert(
            id,
            PeerHandle {
                signals: signal_tx,
                task,
            },
        );
    }

    fn add_answerer(&mut self, id: String, sdp: String) {
        let (signal_tx, signal_rx) = mpsc::unbounded_channel();
        let client = self.client.clone();
        let local = self.local.subscribe();
        let video = self.video.subscribe();
        let sink = self.sink.clone();
        let events = self.events.clone();
        let playout = self.handle.playout.clone();
        let down = self.down.clone();
        let up = self.up.clone();
        let turn = self.turn.clone();
        let peer_id = id.clone();
        let task = tokio::spawn(async move {
            let result = run_peer(
                peer_id.clone(),
                Offer::Answer(sdp),
                client.clone(),
                local,
                video,
                signal_rx,
                playout,
                sink,
                events,
                turn,
                up,
            )
            .await;
            if result.is_err() {
                let _ = client.send_signal(&peer_id, json!({ "kind": "bye" }));
            }
            let _ = down.send((peer_id, result.err()));
        });
        self.peers.insert(
            id,
            PeerHandle {
                signals: signal_tx,
                task,
            },
        );
    }

    fn route_signal(&mut self, signal: IncomingSignal) {
        let from = signal.from;
        let kind = signal
            .signal
            .get("kind")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        match kind {
            "offer" => {
                let Some(sdp) = signal.signal.get("sdp").and_then(|value| value.as_str()) else {
                    return;
                };
                if self.peers.contains_key(&from) {
                    // The designated offerer already owns this pair.
                    return;
                }
                self.add_answerer(from, sdp.to_owned());
            }
            "answer" => {
                let Some(sdp) = signal.signal.get("sdp").and_then(|value| value.as_str()) else {
                    return;
                };
                if let Some(peer) = self.peers.get(&from) {
                    let _ = peer.signals.send(PeerSignal::Answer(sdp.to_owned()));
                }
            }
            "bye" => {
                if let Some(peer) = self.peers.remove(&from) {
                    peer.task.abort();
                }
            }
            "need-keyframe" => {
                self.force_keyframe.store(true, Ordering::Relaxed);
            }
            _ => {}
        }
    }

    fn shutdown(&mut self) {
        for (id, peer) in self.peers.drain() {
            self.client.send_signal(&id, json!({ "kind": "bye" }));
            peer.task.abort();
        }
        let _ = self.share.send(false);
        self.sink.clear();
        self.client.set_presence(None, None, Some(false));
        self.client.set_voice(false);
        let _ = self.events.send(VoiceEvent::Ended);
    }
}

enum Offer {
    Create,
    Answer(String),
}

#[allow(clippy::too_many_arguments)]
async fn run_peer(
    id: String,
    offer: Offer,
    client: ClientHandle,
    mut local: broadcast::Receiver<Vec<i16>>,
    mut video: broadcast::Receiver<Arc<Vec<u8>>>,
    mut signals: mpsc::UnboundedReceiver<PeerSignal>,
    playout: Playout,
    sink: VideoSink,
    events: mpsc::UnboundedSender<VoiceEvent>,
    turn: Option<TurnCredentials>,
    up: mpsc::UnboundedSender<String>,
) -> Result<(), String> {
    // `0.0.0.0:0` gathers one host candidate per interface, which is enough on a LAN.
    let bind = std::env::var("RUST_CHAT_MEDIA_UDP").unwrap_or_else(|_| "0.0.0.0:0".to_owned());
    let ice = turn.as_ref().map(turn_ice_servers).unwrap_or_default();
    let mut peer = VoicePeer::new(&bind, ice)
        .await
        .map_err(|error| format!("voice peer: {error}"))?;

    match offer {
        Offer::Create => {
            let sdp = peer
                .create_offer()
                .await
                .map_err(|error| format!("offer: {error}"))?;
            client.send_signal(&id, json!({ "kind": "offer", "sdp": sdp }));
            let answer = tokio::time::timeout(CONNECT_TIMEOUT, signals.recv())
                .await
                .map_err(|_| "the peer did not answer".to_owned())?
                .ok_or_else(|| "the peer went away".to_owned())?;
            match answer {
                PeerSignal::Answer(sdp) => peer
                    .accept_answer(&sdp)
                    .await
                    .map_err(|error| format!("answer: {error}"))?,
            }
        }
        Offer::Answer(sdp) => {
            let answer = peer
                .accept_offer(&sdp)
                .await
                .map_err(|error| format!("answer: {error}"))?;
            client.send_signal(&id, json!({ "kind": "answer", "sdp": answer }));
        }
    }

    peer.wait_connected(CONNECT_TIMEOUT)
        .await
        .map_err(|error| format!("voice connection: {error}"))?;
    peer.prepare()
        .await
        .map_err(|error| format!("voice media: {error}"))?;
    let mut received = peer
        .take_received()
        .ok_or_else(|| "the peer had no receiver".to_owned())?;
    let mut received_video = peer
        .take_received_video()
        .ok_or_else(|| "the peer had no video receiver".to_owned())?;
    let mut video_lost = peer
        .take_video_lost()
        .ok_or_else(|| "the peer had no video loss signal".to_owned())?;
    let _ = up.send(id.clone());
    let _ = events.send(VoiceEvent::Ready(id.clone()));
    // A viewer joining mid-share should not wait for the periodic keyframe.
    client.send_signal(&id, json!({ "kind": "need-keyframe" }));

    let mut last_keyframe = Instant::now();
    let mut health = tokio::time::interval(Duration::from_secs(1));
    health.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut stats_tick = tokio::time::interval(Duration::from_secs(2));
    stats_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_level = Instant::now() - LEVEL_INTERVAL;
    loop {
        tokio::select! {
            _ = stats_tick.tick() => {
                let stats = peer.link_stats().await;
                let _ = events.send(VoiceEvent::Stats {
                    peer: id.clone(),
                    rtt_ms: stats.rtt_ms,
                    relayed: stats.relayed,
                });
            }
            _ = health.tick() => {
                if peer.is_failed() {
                    return Err("the peer connection was lost".to_owned());
                }
            }
            _ = video_lost.recv() => {
                if last_keyframe.elapsed() >= Duration::from_secs(2) {
                    last_keyframe = Instant::now();
                    client.send_signal(&id, json!({ "kind": "need-keyframe" }));
                }
            }
            frame = local.recv() => match frame {
                Ok(frame) => {
                    if let Err(error) = peer.send_pcm(&frame).await {
                        return Err(format!("send: {error}"));
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            },
            frame = video.recv() => match frame {
                Ok(frame) => {
                    if let Err(error) = peer
                        .send_video(&frame, Duration::from_millis(1000 / SHARE_FPS as u64))
                        .await
                    {
                        return Err(format!("send video: {error}"));
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            },
            signal = signals.recv() => match signal {
                Some(PeerSignal::Answer(_)) => {}
                None => break,
            },
            pcm = received.recv() => match pcm {
                Some(pcm) => {
                    if last_level.elapsed() >= LEVEL_INTERVAL {
                        last_level = Instant::now();
                        let _ = events.send(VoiceEvent::Level {
                            peer: id.clone(),
                            level: rms(&pcm),
                        });
                    }
                    playout.push(&pcm);
                }
                None => break,
            },
            picture = received_video.recv() => match picture {
                Some(picture) => {
                    let bgra = i420_to_bgra(&picture.i420, picture.width, picture.height);
                    sink.publish(VideoFrame {
                        width: picture.width,
                        height: picture.height,
                        bgra,
                        at: Instant::now(),
                    });
                }
                None => break,
            },
        }
    }
    client.send_signal(&id, json!({ "kind": "bye" }));
    peer.close().await;
    Ok(())
}

/// Owns the capture thread and the VP8 encoder away from the audio loop.
async fn share_task(
    mut commands: mpsc::UnboundedReceiver<bool>,
    capture_tx: mpsc::UnboundedSender<CapturedFrame>,
    mut capture_rx: mpsc::UnboundedReceiver<CapturedFrame>,
    video: broadcast::Sender<Arc<Vec<u8>>>,
    events: mpsc::UnboundedSender<VoiceEvent>,
    force_keyframe: Arc<AtomicBool>,
) {
    let mut capture: Option<ScreenCapture> = None;
    let mut encoder: Option<VideoEncoder> = None;
    let mut last = Instant::now() - SHARE_INTERVAL;
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(true) => {
                    if capture.is_none() {
                        match ScreenCapture::start_monitor(1, capture_tx.clone()) {
                            Ok(started) => capture = Some(started),
                            Err(error) => {
                                let _ = events.send(VoiceEvent::PeerFailed(format!(
                                    "screen capture: {error}"
                                )));
                            }
                        }
                    }
                }
                Some(false) | None => {
                    capture = None;
                    encoder = None;
                }
            },
            frame = capture_rx.recv() => {
                let Some(frame) = frame else { break };
                if capture.is_none() || last.elapsed() < SHARE_INTERVAL {
                    continue;
                }
                let (bgra, width, height) = fit_bgra(
                    &frame.bgra,
                    frame.width,
                    frame.height,
                    SHARE_MAX_WIDTH,
                    SHARE_MAX_HEIGHT,
                );
                if encoder
                    .as_ref()
                    .is_none_or(|encoder| encoder.dimensions() != (width, height))
                {
                    encoder = VideoEncoder::new(width, height, SHARE_FPS, SHARE_BITRATE).ok();
                }
                let Some(encoder) = encoder.as_mut() else {
                    continue;
                };
                let i420 = bgra_to_i420(&bgra, width, height);
                let keyframe = force_keyframe.swap(false, Ordering::Relaxed);
                match encoder.encode(&i420, keyframe) {
                    Ok(Some(frame)) => {
                        let _ = video.send(Arc::new(frame.data));
                        last = Instant::now();
                    }
                    Ok(None) => last = Instant::now(),
                    Err(error) => {
                        let _ = events.send(VoiceEvent::PeerFailed(format!("VP8 encode: {error}")));
                        last = Instant::now();
                    }
                }
            }
        }
    }
    if let Some(mut capture) = capture {
        capture.stop();
    }
}
