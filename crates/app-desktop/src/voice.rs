//! Per-peer voice mesh on top of the WebSocket signal relay.
//!
//! The room is the mesh: every member with `voice = true` gets one WebRTC peer
//! connection with an Opus track. To avoid offer glare, the member with the smaller
//! session id creates the offer; the other side answers. SDP carries its host
//! candidates inline, so no trickle ICE is needed.

use rust_chat_client_core::{ClientHandle, IncomingSignal};
use rust_chat_media::audio::{FRAME_SAMPLES, rms};
use rust_chat_media::voice::VoicePeer;
use rust_chat_media_desktop::audio::{AudioEvent, AudioHandle, Playout};
use serde_json::json;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(25);
const LEVEL_INTERVAL: Duration = Duration::from_millis(150);

#[derive(Debug)]
pub enum VoiceCommand {
    /// The current set of members that announce `voice = true` (including self).
    Members(Vec<String>),
    MicMuted(bool),
    SpeakerMuted(bool),
    Stop,
}

#[derive(Debug)]
pub enum VoiceEvent {
    Ready,
    Level { peer: String, level: f32 },
    SelfLevel(f32),
    PeerFailed(String),
    Ended,
    Error(String),
}

pub struct VoiceController {
    commands: mpsc::UnboundedSender<VoiceCommand>,
    events: mpsc::UnboundedReceiver<VoiceEvent>,
}

impl VoiceController {
    pub fn send(&self, command: VoiceCommand) {
        let _ = self.commands.send(command);
    }

    pub fn try_recv(&mut self) -> Option<VoiceEvent> {
        self.events.try_recv().ok()
    }
}

/// Starts the controller. The returned value keeps the command channel open; dropping it
/// ends the mesh.
pub fn start(
    client: ClientHandle,
    self_id: String,
    handle: AudioHandle,
    runtime: &tokio::runtime::Handle,
) -> VoiceController {
    let signals = client.subscribe_signals();
    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let (local_tx, _) = broadcast::channel::<Vec<i16>>(16);
    let run = VoiceRun {
        client,
        self_id,
        handle,
        signals,
        commands: command_rx,
        events: event_tx,
        peers: HashMap::new(),
        local: local_tx,
        mic_muted: true,
        speaker_muted: false,
        last_self_level: Instant::now() - LEVEL_INTERVAL,
    };
    runtime.spawn(run.run());
    VoiceController {
        commands: command_tx,
        events: event_rx,
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
    local: broadcast::Sender<Vec<i16>>,
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
                    Some(VoiceCommand::Stop) | None => break,
                },
                signal = self.signals.recv() => match signal {
                    Ok(signal) => self.route_signal(signal),
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                },
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

    fn add_offerer(&mut self, id: String) {
        let (signal_tx, signal_rx) = mpsc::unbounded_channel();
        let client = self.client.clone();
        let local = self.local.subscribe();
        let events = self.events.clone();
        let playout = self.handle.playout.clone();
        let peer_id = id.clone();
        let task = tokio::spawn(async move {
            if let Err(error) = run_peer(
                peer_id.clone(),
                Offer::Create,
                client.clone(),
                local,
                signal_rx,
                playout,
                events.clone(),
            )
            .await
            {
                let _ = client.send_signal(&peer_id, json!({ "kind": "bye" }));
                let _ = events.send(VoiceEvent::PeerFailed(error));
            }
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
        let events = self.events.clone();
        let playout = self.handle.playout.clone();
        let peer_id = id.clone();
        let task = tokio::spawn(async move {
            if let Err(error) = run_peer(
                peer_id.clone(),
                Offer::Answer(sdp),
                client.clone(),
                local,
                signal_rx,
                playout,
                events.clone(),
            )
            .await
            {
                let _ = client.send_signal(&peer_id, json!({ "kind": "bye" }));
                let _ = events.send(VoiceEvent::PeerFailed(error));
            }
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
            _ => {}
        }
    }

    fn shutdown(&mut self) {
        for (id, peer) in self.peers.drain() {
            self.client.send_signal(&id, json!({ "kind": "bye" }));
            peer.task.abort();
        }
        self.client.set_voice(false);
        let _ = self.events.send(VoiceEvent::Ended);
    }
}

enum Offer {
    Create,
    Answer(String),
}

async fn run_peer(
    id: String,
    offer: Offer,
    client: ClientHandle,
    mut local: broadcast::Receiver<Vec<i16>>,
    mut signals: mpsc::UnboundedReceiver<PeerSignal>,
    playout: Playout,
    events: mpsc::UnboundedSender<VoiceEvent>,
) -> Result<(), String> {
    // `0.0.0.0:0` gathers one host candidate per interface, which is enough on a LAN.
    let bind = std::env::var("RUST_CHAT_MEDIA_UDP").unwrap_or_else(|_| "0.0.0.0:0".to_owned());
    let mut peer = VoicePeer::new(&bind)
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
    let _ = events.send(VoiceEvent::Ready);

    let mut last_level = Instant::now() - LEVEL_INTERVAL;
    loop {
        tokio::select! {
            frame = local.recv() => match frame {
                Ok(frame) => {
                    if let Err(error) = peer.send_pcm(&frame).await {
                        return Err(format!("send: {error}"));
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
        }
    }
    client.send_signal(&id, json!({ "kind": "bye" }));
    peer.close().await;
    Ok(())
}
