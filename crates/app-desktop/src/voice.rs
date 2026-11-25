//! Per-peer voice mesh on top of the WebSocket signal relay.
//!
//! The room is the mesh: every member with `voice = true` gets one WebRTC peer
//! connection with an Opus track. To avoid offer glare, the member with the smaller
//! session id creates the offer; the other side answers. SDP carries its host
//! candidates inline, so no trickle ICE is needed.

use crate::control::{
    ControlMessage, HEARTBEAT_SECS, HEARTBEAT_TIMEOUT_SECS, InputEvent, RemoteButton, lease_secs,
};
use crate::files::{
    CHUNK_BYTES, FileFrame, FileMessage, MAX_FILE_BYTES, MAX_FILE_NAME_CHARS, decode_frame,
    encode_chunk, human_size, sanitize_file_name, sha256_file, unique_path,
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
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
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
    SendFile {
        peer: String,
        path: String,
        caption: String,
    },
    AcceptFile {
        id: String,
    },
    DeclineFile {
        id: String,
    },
    CancelFile {
        id: String,
    },
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
    FilePreparing {
        id: String,
        name: String,
    },
    FileOutgoing {
        id: String,
        peer: String,
        name: String,
        size: u64,
    },
    FileOffered {
        id: String,
        peer: String,
        name: String,
        size: u64,
        sha256: String,
        caption: String,
    },
    FileProgress {
        id: String,
        transferred: u64,
        total: u64,
    },
    FileSent {
        id: String,
        path: String,
    },
    FileReceived {
        id: String,
        path: String,
    },
    FileFailed {
        id: String,
        message: String,
    },
    FileCancelled {
        id: String,
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
    file_prefs: std::sync::Arc<crate::files::FilePrefs>,
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
    let (files_in_tx, files_in_rx) = mpsc::unbounded_channel();
    let (prepared_tx, prepared_rx) = mpsc::unbounded_channel();
    let files_shutdown = Arc::new(AtomicBool::new(false));
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
        files_in: files_in_tx,
        files_out: files_in_rx,
        prepared: prepared_tx,
        prepared_rx,
        transfers: HashMap::new(),
        files_shutdown,
        file_prefs,
        last_file: None,
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
    files: mpsc::Sender<FileOut>,
    task: JoinHandle<()>,
}

enum FileOut {
    Message(FileMessage),
    Binary(Vec<u8>),
}

struct PreparedFile {
    id: String,
    peer: String,
    path: PathBuf,
    name: String,
    size: u64,
    sha256: Result<String, String>,
    caption: String,
}

enum Transfer {
    Awaiting {
        peer: String,
        path: PathBuf,
    },
    Outgoing {
        peer: String,
        cancel: Arc<AtomicBool>,
    },
    RemoteOffer {
        peer: String,
        name: String,
        size: u64,
        sha256: String,
    },
    Incoming {
        peer: String,
        part: PathBuf,
        final_path: PathBuf,
        file: std::fs::File,
        hasher: Sha256,
        size: u64,
        sha256: String,
        received: u64,
        last_report: Instant,
    },
}

impl Transfer {
    fn peer(&self) -> &str {
        match self {
            Self::Awaiting { peer, .. }
            | Self::Outgoing { peer, .. }
            | Self::RemoteOffer { peer, .. }
            | Self::Incoming { peer, .. } => peer,
        }
    }
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
    files_in: mpsc::UnboundedSender<(String, Vec<u8>)>,
    files_out: mpsc::UnboundedReceiver<(String, Vec<u8>)>,
    prepared: mpsc::UnboundedSender<PreparedFile>,
    prepared_rx: mpsc::UnboundedReceiver<PreparedFile>,
    transfers: HashMap<String, Transfer>,
    files_shutdown: Arc<AtomicBool>,
    file_prefs: std::sync::Arc<crate::files::FilePrefs>,
    last_file: Option<PathBuf>,
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
                    Some(VoiceCommand::SendFile {
                        peer,
                        path,
                        caption,
                    }) => self.send_file(peer, path, caption),
                    Some(VoiceCommand::AcceptFile { id }) => self.accept_file(id),
                    Some(VoiceCommand::DeclineFile { id }) => self.decline_file(id),
                    Some(VoiceCommand::CancelFile { id }) => self.cancel_file(id),
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
                prepared = self.prepared_rx.recv() => {
                    if let Some(prepared) = prepared {
                        self.on_prepared(prepared);
                    }
                }
                frame = self.files_out.recv() => {
                    if let Some((from, frame)) = frame {
                        self.on_file_frame(from, frame);
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
        let failed: Vec<String> = self
            .transfers
            .iter()
            .filter(|(_, transfer)| transfer.peer() == id)
            .map(|(transfer_id, _)| transfer_id.clone())
            .collect();
        for transfer_id in failed {
            self.fail_transfer(&transfer_id, "the peer disconnected");
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
        let (files_tx, files_rx) = mpsc::channel(128);
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
        let files_in = self.files_in.clone();
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
                files_in,
                files_rx,
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
                files: files_tx,
                task,
            },
        );
    }

    fn add_answerer(&mut self, id: String, sdp: String) {
        let (signal_tx, signal_rx) = mpsc::unbounded_channel();
        let (control_tx, control_rx) = mpsc::unbounded_channel();
        let (files_tx, files_rx) = mpsc::channel(128);
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
        let files_in = self.files_in.clone();
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
                files_in,
                files_rx,
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
                files: files_tx,
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
        self.cancel_all_files();
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

    fn send_file_message(&self, peer: &str, message: FileMessage) {
        if let Some(handle) = self.peers.get(peer) {
            let _ = handle.files.try_send(FileOut::Message(message));
        }
    }

    fn send_file(&mut self, peer: String, path: String, caption: String) {
        let caption: String = caption.chars().take(200).collect();
        let path = PathBuf::from(path);
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => metadata,
            Ok(_) => {
                let _ = self.events.send(VoiceEvent::FileFailed {
                    id: String::new(),
                    message: "that path is not a file".into(),
                });
                return;
            }
            Err(error) => {
                let _ = self.events.send(VoiceEvent::FileFailed {
                    id: String::new(),
                    message: format!("cannot open {path:?}: {error}"),
                });
                return;
            }
        };
        let size = metadata.len();
        if size > MAX_FILE_BYTES {
            let _ = self.events.send(VoiceEvent::FileFailed {
                id: String::new(),
                message: format!("files are limited to {}", human_size(MAX_FILE_BYTES)),
            });
            return;
        }
        let Some(name) = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .and_then(|name| sanitize_file_name(&name))
        else {
            let _ = self.events.send(VoiceEvent::FileFailed {
                id: String::new(),
                message: "the file name is not valid".into(),
            });
            return;
        };
        if name.chars().count() > MAX_FILE_NAME_CHARS {
            let _ = self.events.send(VoiceEvent::FileFailed {
                id: String::new(),
                message: "the file name is too long".into(),
            });
            return;
        }
        let id = format!("f-{:016x}", rand::random::<u64>());
        let _ = self.events.send(VoiceEvent::FilePreparing {
            id: id.clone(),
            name: name.clone(),
        });
        let prepared = self.prepared.clone();
        let shutdown = self.files_shutdown.clone();
        tokio::task::spawn_blocking(move || {
            if shutdown.load(Ordering::Relaxed) {
                return;
            }
            let sha256 = sha256_file(&path);
            let _ = prepared.send(PreparedFile {
                id,
                peer,
                path,
                name,
                size,
                sha256,
                caption,
            });
        });
    }

    fn on_prepared(&mut self, prepared: PreparedFile) {
        match prepared.sha256 {
            Ok(sha256) => {
                self.transfers.insert(
                    prepared.id.clone(),
                    Transfer::Awaiting {
                        peer: prepared.peer.clone(),
                        path: prepared.path.clone(),
                    },
                );
                self.send_file_message(
                    &prepared.peer,
                    FileMessage::Offer {
                        id: prepared.id.clone(),
                        name: prepared.name.clone(),
                        size: prepared.size,
                        sha256,
                        caption: prepared.caption,
                    },
                );
                let _ = self.events.send(VoiceEvent::FileOutgoing {
                    id: prepared.id,
                    peer: prepared.peer,
                    name: prepared.name,
                    size: prepared.size,
                });
            }
            Err(message) => {
                let _ = self.events.send(VoiceEvent::FileFailed {
                    id: prepared.id,
                    message,
                });
            }
        }
    }

    fn accept_file(&mut self, id: String) {
        let Some(Transfer::RemoteOffer {
            peer,
            name,
            size,
            sha256,
        }) = self.transfers.remove(&id)
        else {
            return;
        };
        let dir = match self.file_prefs.downloads_dir() {
            Ok(dir) => dir,
            Err(message) => {
                self.fail_offer(&id, &peer, &message);
                return;
            }
        };
        let Some(name) = sanitize_file_name(&name) else {
            self.fail_offer(&id, &peer, "the file name is not valid");
            return;
        };
        let final_path = unique_path(&dir, &name);
        let part = PathBuf::from(format!("{}.part", final_path.display()));
        let file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&part)
        {
            Ok(file) => file,
            Err(error) => {
                self.fail_offer(&id, &peer, &format!("cannot create the download: {error}"));
                return;
            }
        };
        self.transfers.insert(
            id.clone(),
            Transfer::Incoming {
                peer: peer.clone(),
                part,
                final_path,
                file,
                hasher: Sha256::new(),
                size,
                sha256,
                received: 0,
                last_report: Instant::now() - Duration::from_secs(1),
            },
        );
        self.send_file_message(&peer, FileMessage::Accept { id: id.clone() });
        let _ = self.events.send(VoiceEvent::FileProgress {
            id,
            transferred: 0,
            total: size,
        });
    }

    fn fail_offer(&self, id: &str, peer: &str, message: &str) {
        self.send_file_message(
            peer,
            FileMessage::Decline {
                id: id.to_owned(),
                reason: message.to_owned(),
            },
        );
        let _ = self.events.send(VoiceEvent::FileFailed {
            id: id.to_owned(),
            message: message.to_owned(),
        });
    }

    fn decline_file(&mut self, id: String) {
        if let Some(transfer) = self.transfers.remove(&id) {
            let peer = transfer.peer().to_owned();
            self.send_file_message(
                &peer,
                FileMessage::Decline {
                    id: id.clone(),
                    reason: "declined".into(),
                },
            );
            let _ = self.events.send(VoiceEvent::FileCancelled { id });
        }
    }

    fn cancel_file(&mut self, id: String) {
        let Some(transfer) = self.transfers.remove(&id) else {
            return;
        };
        let peer = transfer.peer().to_owned();
        match transfer {
            Transfer::Incoming { part, file, .. } => {
                drop(file);
                let _ = std::fs::remove_file(part);
            }
            Transfer::Outgoing { cancel, .. } => {
                cancel.store(true, Ordering::Relaxed);
            }
            _ => {}
        }
        self.send_file_message(
            &peer,
            FileMessage::Cancel {
                id: id.clone(),
                reason: "cancelled".into(),
            },
        );
        let _ = self.events.send(VoiceEvent::FileCancelled { id });
    }

    fn fail_transfer(&mut self, id: &str, message: &str) {
        let Some(transfer) = self.transfers.remove(id) else {
            return;
        };
        let peer = transfer.peer().to_owned();
        if let Transfer::Incoming { part, file, .. } = transfer {
            drop(file);
            let _ = std::fs::remove_file(part);
        }
        self.send_file_message(
            &peer,
            FileMessage::Error {
                id: id.to_owned(),
                message: message.to_owned(),
            },
        );
        let _ = self.events.send(VoiceEvent::FileFailed {
            id: id.to_owned(),
            message: message.to_owned(),
        });
    }

    fn on_file_frame(&mut self, peer: String, bytes: Vec<u8>) {
        let Some(frame) = decode_frame(&bytes) else {
            return;
        };
        match frame {
            FileFrame::Message(message) => self.on_file_message(peer, message),
            FileFrame::Chunk { id, offset, data } => self.on_file_chunk(peer, id, offset, data),
        }
    }

    fn on_file_message(&mut self, peer: String, message: FileMessage) {
        match message {
            FileMessage::Offer {
                id,
                name,
                size,
                sha256,
                caption,
            } => {
                let caption: String = caption.chars().take(200).collect();
                if size > MAX_FILE_BYTES
                    || name.chars().count() > MAX_FILE_NAME_CHARS
                    || sha256.len() != 64
                    || sanitize_file_name(&name).is_none()
                {
                    self.send_file_message(
                        &peer,
                        FileMessage::Decline {
                            id,
                            reason: "invalid offer".into(),
                        },
                    );
                    return;
                }
                if self.transfers.values().any(|transfer| {
                    transfer.peer() == peer
                        && matches!(
                            transfer,
                            Transfer::RemoteOffer { .. } | Transfer::Incoming { .. }
                        )
                }) {
                    self.send_file_message(
                        &peer,
                        FileMessage::Decline {
                            id,
                            reason: "another transfer is already active".into(),
                        },
                    );
                    return;
                }
                self.transfers.insert(
                    id.clone(),
                    Transfer::RemoteOffer {
                        peer: peer.clone(),
                        name: name.clone(),
                        size,
                        sha256: sha256.clone(),
                    },
                );
                let _ = self.events.send(VoiceEvent::FileOffered {
                    id,
                    peer,
                    name,
                    size,
                    sha256,
                    caption,
                });
            }
            FileMessage::Accept { id } => {
                let Some(Transfer::Awaiting { peer: target, path }) = self.transfers.remove(&id)
                else {
                    return;
                };
                if target != peer {
                    return;
                }
                let Some(handle) = self.peers.get(&peer) else {
                    self.fail_transfer(&id, "the peer went away");
                    return;
                };
                let tx = handle.files.clone();
                let cancel = Arc::new(AtomicBool::new(false));
                self.transfers.insert(
                    id.clone(),
                    Transfer::Outgoing {
                        peer: peer.clone(),
                        cancel: cancel.clone(),
                    },
                );
                let events = self.events.clone();
                let shutdown = self.files_shutdown.clone();
                let transfer_id = id.clone();
                tokio::spawn(stream_file(transfer_id, path, tx, cancel, shutdown, events));
            }
            FileMessage::Decline { id, reason } => {
                if self.transfers.remove(&id).is_some() {
                    let _ = self.events.send(VoiceEvent::FileFailed {
                        id,
                        message: if reason.is_empty() {
                            "declined".to_owned()
                        } else {
                            format!("declined: {reason}")
                        },
                    });
                }
            }
            FileMessage::Cancel { id, reason } => {
                let Some(transfer) = self.transfers.remove(&id) else {
                    return;
                };
                if let Transfer::Incoming { part, file, .. } = transfer {
                    drop(file);
                    let _ = std::fs::remove_file(part);
                }
                let _ = self.events.send(VoiceEvent::FileFailed {
                    id,
                    message: format!("cancelled: {reason}"),
                });
            }
            FileMessage::Finish { id } => self.finish_incoming(id),
            FileMessage::Complete { id } => {
                self.transfers.remove(&id);
            }
            FileMessage::Error { id, message } => {
                if let Some(Transfer::Incoming { part, file, .. }) = self.transfers.remove(&id) {
                    drop(file);
                    let _ = std::fs::remove_file(part);
                }
                let _ = self.events.send(VoiceEvent::FileFailed { id, message });
            }
        }
    }

    fn on_file_chunk(&mut self, peer: String, id: String, offset: u64, data: Vec<u8>) {
        let failure = {
            let Some(Transfer::Incoming {
                peer: owner,
                file,
                hasher,
                size,
                received,
                last_report,
                ..
            }) = self.transfers.get_mut(&id)
            else {
                return;
            };
            if owner != &peer {
                return;
            }
            if offset != *received {
                Some(format!("chunks arrived out of order at {offset}"))
            } else if file.write_all(&data).is_err() {
                Some("cannot write the download".to_owned())
            } else {
                hasher.update(&data);
                *received += data.len() as u64;
                let total = *size;
                let done = *received;
                if last_report.elapsed() >= Duration::from_millis(100) || done >= total {
                    *last_report = Instant::now();
                    let _ = self.events.send(VoiceEvent::FileProgress {
                        id: id.clone(),
                        transferred: done,
                        total,
                    });
                }
                None
            }
        };
        if let Some(message) = failure {
            self.fail_transfer(&id, &message);
        }
    }

    fn finish_incoming(&mut self, id: String) {
        let Some(Transfer::Incoming {
            peer,
            mut file,
            hasher,
            part,
            final_path,
            size,
            sha256,
            received,
            ..
        }) = self.transfers.remove(&id)
        else {
            return;
        };
        let _ = file.flush();
        drop(file);
        let actual = crate::files::hex(&hasher.finalize());
        if received != size || actual != sha256 {
            let _ = std::fs::remove_file(&part);
            self.send_file_message(
                &peer,
                FileMessage::Error {
                    id: id.clone(),
                    message: "checksum or size mismatch".into(),
                },
            );
            let _ = self.events.send(VoiceEvent::FileFailed {
                id,
                message: "checksum or size mismatch".into(),
            });
            return;
        }
        match std::fs::rename(&part, &final_path) {
            Ok(()) => {
                self.last_file = Some(final_path.clone());
                self.send_file_message(&peer, FileMessage::Complete { id: id.clone() });
                let _ = self.events.send(VoiceEvent::FileReceived {
                    id,
                    path: final_path.display().to_string(),
                });
            }
            Err(error) => {
                let _ = std::fs::remove_file(&part);
                self.send_file_message(
                    &peer,
                    FileMessage::Error {
                        id: id.clone(),
                        message: format!("cannot finish the download: {error}"),
                    },
                );
                let _ = self.events.send(VoiceEvent::FileFailed {
                    id,
                    message: format!("cannot finish the download: {error}"),
                });
            }
        }
    }

    fn cancel_all_files(&mut self) {
        self.files_shutdown.store(true, Ordering::Relaxed);
        let ids: Vec<String> = self.transfers.keys().cloned().collect();
        for id in ids {
            if let Some(transfer) = self.transfers.remove(&id) {
                let peer = transfer.peer().to_owned();
                if let Transfer::Incoming { part, file, .. } = transfer {
                    drop(file);
                    let _ = std::fs::remove_file(part);
                }
                self.send_file_message(
                    &peer,
                    FileMessage::Cancel {
                        id,
                        reason: "voice ended".into(),
                    },
                );
            }
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
    files_in: mpsc::UnboundedSender<(String, Vec<u8>)>,
    mut files_out: mpsc::Receiver<FileOut>,
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
            peer.create_files()
                .await
                .map_err(|error| format!("files channel: {error}"))?;
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
    let mut files_rx = peer
        .take_files()
        .ok_or_else(|| "the peer had no files receiver".to_owned())?;
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
            out = files_out.recv() => match out {
                Some(FileOut::Message(message)) => {
                    let data = message.encode();
                    let mut delivered = false;
                    for _ in 0..100 {
                        if peer.send_file_frame(data.clone()).await {
                            delivered = true;
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                    if !delivered {
                        return Err("the files channel did not open".to_owned());
                    }
                }
                Some(FileOut::Binary(frame)) => {
                    if !peer.send_file_frame(frame).await {
                        return Err("the files channel closed".to_owned());
                    }
                }
                None => break,
            },
            incoming = files_rx.recv() => {
                if let Some(frame) = incoming {
                    let _ = files_in.send((id.clone(), frame));
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

async fn stream_file(
    id: String,
    path: PathBuf,
    tx: mpsc::Sender<FileOut>,
    cancel: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    events: mpsc::UnboundedSender<VoiceEvent>,
) {
    let mut file = match tokio::fs::File::open(&path).await {
        Ok(file) => file,
        Err(error) => {
            let _ = events.send(VoiceEvent::FileFailed {
                id,
                message: format!("cannot read the file: {error}"),
            });
            return;
        }
    };
    let size = file.metadata().await.map(|meta| meta.len()).unwrap_or(0);
    let mut buffer = vec![0u8; CHUNK_BYTES];
    let mut offset = 0u64;
    let mut last_report = Instant::now() - Duration::from_secs(1);
    loop {
        if cancel.load(Ordering::Relaxed) || shutdown.load(Ordering::Relaxed) {
            let _ = tx
                .send(FileOut::Message(FileMessage::Cancel {
                    id: id.clone(),
                    reason: "cancelled".into(),
                }))
                .await;
            let _ = events.send(VoiceEvent::FileCancelled { id });
            return;
        }
        let read = match file.read(&mut buffer).await {
            Ok(read) => read,
            Err(error) => {
                let _ = events.send(VoiceEvent::FileFailed {
                    id,
                    message: format!("cannot read the file: {error}"),
                });
                return;
            }
        };
        if read == 0 {
            break;
        }
        let frame = encode_chunk(&id, offset, &buffer[..read]);
        if tx.send(FileOut::Binary(frame)).await.is_err() {
            let _ = events.send(VoiceEvent::FileFailed {
                id,
                message: "the peer went away".into(),
            });
            return;
        }
        offset += read as u64;
        if last_report.elapsed() >= Duration::from_millis(100) || offset >= size {
            last_report = Instant::now();
            let _ = events.send(VoiceEvent::FileProgress {
                id: id.clone(),
                transferred: offset,
                total: size,
            });
        }
    }
    let _ = tx
        .send(FileOut::Message(FileMessage::Finish { id: id.clone() }))
        .await;
    let _ = events.send(VoiceEvent::FileSent {
        id,
        path: path.display().to_string(),
    });
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
                Some(ShareCommand::Stop) => {
                    if let Some(mut old) = capture.take() {
                        old.stop();
                    }
                    encoder = None;
                }
                // The controller is gone: stop capturing and end the task instead of spinning
                // on a closed channel.
                // The controller is gone: stop capturing and end the task instead of spinning
                // on a closed channel.
                None => {
                    if let Some(mut old) = capture.take() {
                        old.stop();
                    }
                    break;
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
