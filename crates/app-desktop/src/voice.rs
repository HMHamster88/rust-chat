//! Per-peer voice mesh on top of the WebSocket signal relay.
//!
//! The room is the mesh: every member with `voice = true` gets one WebRTC peer
//! connection with an Opus track. To avoid offer glare, the member with the smaller
//! session id creates the offer; the other side answers. SDP carries its host
//! candidates inline, so no trickle ICE is needed.

use crate::control::{
    ControlMessage, HEARTBEAT_SECS, HEARTBEAT_TIMEOUT_SECS, InputEvent, RemoteButton, lease_secs,
};
use rust_chat_client_core::{ClientHandle, IncomingSignal};
use rust_chat_media::audio::{FRAME_SAMPLES, rms};
use rust_chat_media::video::{VideoEncoder, bgra_to_i420, fit_bgra, i420_to_bgra};
use rust_chat_media::voice::{VoicePeer, turn_ice_servers};
use rust_chat_media_desktop::audio::{AudioEvent, AudioHandle, Playout};
use rust_chat_media_desktop::capture::{CaptureSource, CapturedFrame, ScreenCapture, capture_rect};
use rust_chat_media_desktop::input::InputInjector;
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

#[derive(Debug, Clone)]
pub struct ShareSettings {
    pub source: CaptureSource,
    pub max_width: u32,
    pub max_height: u32,
    pub fps: u32,
    pub bitrate: u32,
}

impl Default for ShareSettings {
    fn default() -> Self {
        Self {
            source: CaptureSource::Monitor(1),
            max_width: 1280,
            max_height: 720,
            fps: 10,
            bitrate: 1_500_000,
        }
    }
}

#[derive(Debug, Clone)]
struct VideoPacket {
    data: Vec<u8>,
    fps: u32,
}

#[derive(Debug)]
enum ShareCommand {
    Start(ShareSettings),
    Stop,
}

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
    ConfigureShare(ShareSettings),
    ControlRequest {
        peer: String,
        lease_secs: u64,
    },
    ControlRespond {
        peer: String,
        allow: bool,
        lease_secs: u64,
    },
    ControlInput(InputEvent),
    ControlStop,
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
    ControlRequested {
        peer: String,
        lease_secs: u64,
    },
    ControlGranted {
        peer: String,
        lease_secs: u64,
    },
    ControlHosting {
        peer: String,
        lease_secs: u64,
    },
    ControlEnded {
        reason: String,
    },
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
    share: ShareSettings,
) -> VoiceController {
    let signals = client.subscribe_signals();
    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let (local_tx, _) = broadcast::channel::<Vec<i16>>(16);
    let (video_tx, _) = broadcast::channel::<Arc<VideoPacket>>(4);
    let (share_tx, share_rx) = mpsc::unbounded_channel();
    let (capture_tx, capture_rx) = mpsc::unbounded_channel::<CapturedFrame>();
    let (down_tx, down_rx) = mpsc::unbounded_channel();
    let (up_tx, up_rx) = mpsc::unbounded_channel();
    let (wake_tx, wake_rx) = mpsc::unbounded_channel();
    let (control_tx, control_rx) = mpsc::unbounded_channel();
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
        sharing: false,
        control_in: control_tx,
        control_out: control_rx,
        viewer: None,
        viewer_pending: None,
        host: None,
        pending: None,
        injector: None,
        local: local_tx,
        video: video_tx,
        share: share_tx,
        share_settings: share,
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
    control: mpsc::UnboundedSender<ControlMessage>,
    task: JoinHandle<()>,
}

enum PeerSignal {
    Answer(String),
}

struct ViewerControl {
    peer: String,
    until: Instant,
    last_heartbeat: Instant,
}

struct HostControl {
    peer: String,
    until: Instant,
    last_heartbeat: Instant,
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
    sharing: bool,
    control_in: mpsc::UnboundedSender<(String, ControlMessage)>,
    control_out: mpsc::UnboundedReceiver<(String, ControlMessage)>,
    viewer: Option<ViewerControl>,
    viewer_pending: Option<String>,
    host: Option<HostControl>,
    pending: Option<(String, u64)>,
    injector: Option<InputInjector>,
    local: broadcast::Sender<Vec<i16>>,
    video: broadcast::Sender<Arc<VideoPacket>>,
    share: mpsc::UnboundedSender<ShareCommand>,
    share_settings: ShareSettings,
    sink: VideoSink,
    mic_muted: bool,
    speaker_muted: bool,
    last_self_level: Instant,
}

impl VoiceRun {
    async fn run(mut self) {
        self.presence();
        let mut control_tick = tokio::time::interval(Duration::from_secs(1));
        control_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = control_tick.tick() => self.control_tick(),
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
                        self.sharing = on;
                        if on {
                            let _ = self.share.send(ShareCommand::Start(self.share_settings.clone()));
                        } else {
                            let _ = self.share.send(ShareCommand::Stop);
                        }
                        self.client.set_presence(None, None, Some(on));
                        if !on {
                            self.sink.clear();
                            self.stop_control("sharing stopped");
                        }
                    }
                    Some(VoiceCommand::ConfigureShare(settings)) => {
                        self.share_settings = settings;
                        if self.sharing {
                            let _ = self
                                .share
                                .send(ShareCommand::Start(self.share_settings.clone()));
                        }
                    }
                    Some(VoiceCommand::ControlRequest { peer, lease_secs }) => {
                        self.request_control(peer, lease_secs);
                    }
                    Some(VoiceCommand::ControlRespond {
                        peer,
                        allow,
                        lease_secs,
                    }) => self.respond_control(peer, allow, lease_secs),
                    Some(VoiceCommand::ControlInput(event)) => self.control_input(event),
                    Some(VoiceCommand::ControlStop) => self.stop_control("stopped"),
                    Some(VoiceCommand::Stop) | None => break,
                },
                signal = self.signals.recv() => match signal {
                    Ok(signal) => self.route_signal(signal),
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                message = self.control_out.recv() => {
                    if let Some((from, message)) = message {
                        self.on_control(from, message);
                    }
                }
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
        if self.viewer.as_ref().is_some_and(|viewer| viewer.peer == id) {
            self.end_viewer("the host disconnected", false);
        }
        if self.host.as_ref().is_some_and(|host| host.peer == id) {
            self.end_host("the controller disconnected", false);
        }
        if self.pending.as_ref().is_some_and(|(peer, _)| *peer == id) {
            self.pending = None;
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
        let (control_tx, control_rx) = mpsc::unbounded_channel();
        let client = self.client.clone();
        let local = self.local.subscribe();
        let video = self.video.subscribe();
        let sink = self.sink.clone();
        let events = self.events.clone();
        let playout = self.handle.playout.clone();
        let down = self.down.clone();
        let up = self.up.clone();
        let turn = self.turn.clone();
        let control_in = self.control_in.clone();
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
                control_in,
                control_rx,
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
                control: control_tx,
                task,
            },
        );
    }

    fn add_answerer(&mut self, id: String, sdp: String) {
        let (signal_tx, signal_rx) = mpsc::unbounded_channel();
        let (control_tx, control_rx) = mpsc::unbounded_channel();
        let client = self.client.clone();
        let local = self.local.subscribe();
        let video = self.video.subscribe();
        let sink = self.sink.clone();
        let events = self.events.clone();
        let playout = self.handle.playout.clone();
        let down = self.down.clone();
        let up = self.up.clone();
        let turn = self.turn.clone();
        let control_in = self.control_in.clone();
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
                control_in,
                control_rx,
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
                control: control_tx,
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
        self.end_viewer("voice ended", true);
        self.end_host("voice ended", true);
        for (id, peer) in self.peers.drain() {
            self.client.send_signal(&id, json!({ "kind": "bye" }));
            peer.task.abort();
        }
        let _ = self.share.send(ShareCommand::Stop);
        self.sink.clear();
        self.client.set_presence(None, None, Some(false));
        self.client.set_voice(false);
        let _ = self.events.send(VoiceEvent::Ended);
    }

    fn send_control(&self, peer: &str, message: ControlMessage) {
        if let Some(handle) = self.peers.get(peer) {
            let _ = handle.control.send(message);
        }
    }

    fn request_control(&mut self, peer: String, requested: u64) {
        if self.viewer.is_some() || self.viewer_pending.is_some() || self.host.is_some() {
            return;
        }
        let secs = lease_secs(requested);
        self.send_control(&peer, ControlMessage::Request { lease_secs: secs });
        self.viewer_pending = Some(peer);
    }

    fn respond_control(&mut self, peer: String, allow: bool, requested: u64) {
        let Some((pending, _)) = self.pending.clone() else {
            return;
        };
        if pending != peer {
            return;
        }
        self.pending = None;
        if !allow {
            self.send_control(
                &peer,
                ControlMessage::Deny {
                    reason: "declined".into(),
                },
            );
            let _ = self.events.send(VoiceEvent::ControlEnded {
                reason: "declined".into(),
            });
            return;
        }
        let rect = match capture_rect(&self.share_settings.source) {
            Ok(rect) => rect,
            Err(error) => {
                self.send_control(
                    &peer,
                    ControlMessage::Deny {
                        reason: format!("cannot map the monitor: {error}"),
                    },
                );
                return;
            }
        };
        let secs = lease_secs(requested);
        self.injector = Some(
            if std::env::var("RUST_CHAT_CONTROL_DRY_RUN").as_deref() == Ok("1") {
                InputInjector::dry_run(rect)
            } else {
                InputInjector::new(rect)
            },
        );
        self.host = Some(HostControl {
            peer: peer.clone(),
            until: Instant::now() + Duration::from_secs(secs),
            last_heartbeat: Instant::now(),
        });
        self.send_control(&peer, ControlMessage::Grant { lease_secs: secs });
        let _ = self.events.send(VoiceEvent::ControlHosting {
            peer,
            lease_secs: secs,
        });
    }

    fn on_control(&mut self, from: String, message: ControlMessage) {
        let now = Instant::now();
        match message {
            ControlMessage::Request {
                lease_secs: requested,
            } => {
                if !self.sharing || self.host.is_some() || self.pending.is_some() {
                    let reason = if self.sharing {
                        "a controller is already connected"
                    } else {
                        "the host is not sharing"
                    };
                    self.send_control(
                        &from,
                        ControlMessage::Deny {
                            reason: reason.into(),
                        },
                    );
                    return;
                }
                let secs = lease_secs(requested);
                self.pending = Some((from.clone(), secs));
                let _ = self.events.send(VoiceEvent::ControlRequested {
                    peer: from,
                    lease_secs: secs,
                });
            }
            ControlMessage::Grant {
                lease_secs: granted,
            } => {
                if self.viewer_pending.as_deref() == Some(from.as_str()) {
                    self.viewer_pending = None;
                    let secs = lease_secs(granted);
                    self.viewer = Some(ViewerControl {
                        peer: from.clone(),
                        until: now + Duration::from_secs(secs),
                        last_heartbeat: now,
                    });
                    let _ = self.events.send(VoiceEvent::ControlGranted {
                        peer: from,
                        lease_secs: secs,
                    });
                }
            }
            ControlMessage::Deny { reason } | ControlMessage::End { reason } => {
                if self.viewer_pending.as_deref() == Some(from.as_str()) {
                    self.viewer_pending = None;
                }
                if self
                    .viewer
                    .as_ref()
                    .is_some_and(|viewer| viewer.peer == from)
                {
                    self.viewer = None;
                    let _ = self.events.send(VoiceEvent::ControlEnded {
                        reason: reason.clone(),
                    });
                }
                if self.host.as_ref().is_some_and(|host| host.peer == from) {
                    self.end_host("the controller stopped", false);
                    let _ = self.events.send(VoiceEvent::ControlEnded { reason });
                }
            }
            ControlMessage::Revoke { reason } => {
                if self
                    .viewer
                    .as_ref()
                    .is_some_and(|viewer| viewer.peer == from)
                {
                    self.viewer = None;
                    self.viewer_pending = None;
                    let _ = self.events.send(VoiceEvent::ControlEnded { reason });
                }
                if self.host.as_ref().is_some_and(|host| host.peer == from) {
                    self.end_host("revoked", false);
                }
            }
            ControlMessage::Heartbeat => {
                if let Some(host) = self.host.as_mut()
                    && host.peer == from
                {
                    host.last_heartbeat = now;
                }
            }
            ControlMessage::Input { event } => {
                let allowed = self
                    .host
                    .as_ref()
                    .is_some_and(|host| host.peer == from && now < host.until);
                if allowed {
                    self.inject(event);
                }
            }
        }
    }

    fn inject(&mut self, event: InputEvent) {
        let Some(injector) = self.injector.as_mut() else {
            return;
        };
        let dry = injector.is_dry_run();
        let start = injector.recorded.len();
        match event {
            InputEvent::MouseMove { x, y } => injector.mouse_move(x, y),
            InputEvent::MouseButton { button, pressed } => {
                let button = match button {
                    RemoteButton::Left => rust_chat_media_desktop::input::MouseButton::Left,
                    RemoteButton::Right => rust_chat_media_desktop::input::MouseButton::Right,
                    RemoteButton::Middle => rust_chat_media_desktop::input::MouseButton::Middle,
                };
                injector.mouse_button(button, pressed);
            }
            InputEvent::Wheel { delta, horizontal } => injector.wheel(delta, horizontal),
            InputEvent::Key { vk, pressed } => {
                injector.key(vk, pressed);
            }
        }
        if dry {
            for event in &injector.recorded[start..] {
                eprintln!("control input: {event}");
            }
        }
    }

    fn control_input(&mut self, event: InputEvent) {
        let Some(viewer) = self.viewer.as_ref() else {
            return;
        };
        if Instant::now() >= viewer.until {
            return;
        }
        let peer = viewer.peer.clone();
        self.send_control(&peer, ControlMessage::Input { event });
    }

    fn stop_control(&mut self, reason: &str) {
        self.end_viewer(reason, true);
        self.end_host(reason, true);
    }

    fn end_viewer(&mut self, reason: &str, notify: bool) {
        if let Some(viewer) = self.viewer.take() {
            if notify {
                self.send_control(
                    &viewer.peer,
                    ControlMessage::End {
                        reason: reason.into(),
                    },
                );
            }
            let _ = self.events.send(VoiceEvent::ControlEnded {
                reason: reason.into(),
            });
        }
        if let Some(pending) = self.viewer_pending.take()
            && notify
        {
            self.send_control(
                &pending,
                ControlMessage::End {
                    reason: reason.into(),
                },
            );
        }
    }

    fn end_host(&mut self, reason: &str, notify: bool) {
        if let Some(host) = self.host.take() {
            if notify {
                self.send_control(
                    &host.peer,
                    ControlMessage::Revoke {
                        reason: reason.into(),
                    },
                );
            }
            if let Some(mut injector) = self.injector.take() {
                injector.release_all();
            }
            let _ = self.events.send(VoiceEvent::ControlEnded {
                reason: reason.into(),
            });
        }
        self.pending = None;
    }

    fn control_tick(&mut self) {
        let now = Instant::now();
        if let Some(viewer) = self.viewer.as_mut() {
            if now >= viewer.until {
                self.end_viewer("lease expired", true);
            } else if viewer.last_heartbeat.elapsed() >= Duration::from_secs(HEARTBEAT_SECS) {
                viewer.last_heartbeat = now;
                let peer = viewer.peer.clone();
                self.send_control(&peer, ControlMessage::Heartbeat);
            }
        }
        if let Some(host) = self.host.as_mut() {
            if now >= host.until {
                self.end_host("lease expired", true);
            } else if host.last_heartbeat.elapsed() > Duration::from_secs(HEARTBEAT_TIMEOUT_SECS) {
                self.end_host("the controller stopped responding", true);
            }
        }
        if let Some((peer, _)) = self.pending.clone()
            && !self.peers.contains_key(&peer)
        {
            self.pending = None;
        }
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
    mut video: broadcast::Receiver<Arc<VideoPacket>>,
    mut signals: mpsc::UnboundedReceiver<PeerSignal>,
    playout: Playout,
    sink: VideoSink,
    events: mpsc::UnboundedSender<VoiceEvent>,
    turn: Option<TurnCredentials>,
    up: mpsc::UnboundedSender<String>,
    control_in: mpsc::UnboundedSender<(String, ControlMessage)>,
    mut control_out: mpsc::UnboundedReceiver<ControlMessage>,
) -> Result<(), String> {
    // `0.0.0.0:0` gathers one host candidate per interface, which is enough on a LAN.
    let bind = std::env::var("RUST_CHAT_MEDIA_UDP").unwrap_or_else(|_| "0.0.0.0:0".to_owned());
    let ice = turn.as_ref().map(turn_ice_servers).unwrap_or_default();
    let mut peer = VoicePeer::new(&bind, ice)
        .await
        .map_err(|error| format!("voice peer: {error}"))?;

    match offer {
        Offer::Create => {
            peer.create_control()
                .await
                .map_err(|error| format!("control channel: {error}"))?;
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
    let mut control_rx = peer
        .take_control()
        .ok_or_else(|| "the peer had no control receiver".to_owned())?;
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
                        .send_video(
                            &frame.data,
                            Duration::from_millis(1000 / frame.fps.max(1) as u64),
                        )
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
            out = control_out.recv() => {
                if let Some(message) = out {
                    let _ = peer.send_control(&message.encode()).await;
                }
            }
            incoming = control_rx.recv() => {
                if let Some(text) = incoming
                    && let Some(message) = ControlMessage::decode(&text)
                {
                    let _ = control_in.send((id.clone(), message));
                }
            }
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
    mut commands: mpsc::UnboundedReceiver<ShareCommand>,
    capture_tx: mpsc::UnboundedSender<CapturedFrame>,
    mut capture_rx: mpsc::UnboundedReceiver<CapturedFrame>,
    video: broadcast::Sender<Arc<VideoPacket>>,
    events: mpsc::UnboundedSender<VoiceEvent>,
    force_keyframe: Arc<AtomicBool>,
) {
    let mut capture: Option<ScreenCapture> = None;
    let mut encoder: Option<VideoEncoder> = None;
    let mut settings = ShareSettings::default();
    let mut last = Instant::now() - Duration::from_millis(100);
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(ShareCommand::Start(next)) => {
                    if let Some(mut old) = capture.take() {
                        old.stop();
                    }
                    encoder = None;
                    while capture_rx.try_recv().is_ok() {}
                    match ScreenCapture::start(&next.source, capture_tx.clone()) {
                        Ok(started) => {
                            settings = next;
                            capture = Some(started);
                            last = Instant::now() - Duration::from_millis(100);
                        }
                        Err(error) => {
                            let _ = events.send(VoiceEvent::PeerFailed(format!(
                                "screen capture: {error}"
                            )));
                        }
                    }
                }
                Some(ShareCommand::Stop) | None => {
                    if let Some(mut old) = capture.take() {
                        old.stop();
                    }
                    encoder = None;
                }
            },
            frame = capture_rx.recv() => {
                let Some(frame) = frame else { break };
                let interval = Duration::from_millis(1000 / settings.fps.max(1) as u64);
                if capture.is_none() || last.elapsed() < interval {
                    continue;
                }
                let (bgra, width, height) = fit_bgra(
                    &frame.bgra,
                    frame.width,
                    frame.height,
                    settings.max_width,
                    settings.max_height,
                );
                if encoder
                    .as_ref()
                    .is_none_or(|encoder| encoder.dimensions() != (width, height))
                {
                    encoder = VideoEncoder::new(
                        width,
                        height,
                        settings.fps,
                        settings.bitrate,
                    )
                    .ok();
                }
                let Some(encoder) = encoder.as_mut() else {
                    continue;
                };
                let i420 = bgra_to_i420(&bgra, width, height);
                let keyframe = force_keyframe.swap(false, Ordering::Relaxed);
                match encoder.encode(&i420, keyframe) {
                    Ok(Some(frame)) => {
                        let _ = video.send(Arc::new(VideoPacket {
                            data: frame.data,
                            fps: settings.fps,
                        }));
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
