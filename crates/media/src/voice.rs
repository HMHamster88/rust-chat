//! WebRTC peer for 48 kHz mono Opus voice and VP8 screen video.

use crate::audio::{AudioError, FRAME_MS, FRAME_SAMPLES, JitterBuffer, OpusDecoder, OpusEncoder};
use crate::video::{DecodedVideo, VideoDecoder, VideoError};
use async_trait::async_trait;
use bytes::BytesMut;
use rtc::media::Sample;
use rtc::media_stream::MediaStreamTrack;
use rtc::peer_connection::configuration::RTCConfigurationBuilder;
use rtc::peer_connection::configuration::interceptor_registry::register_default_interceptors;
use rtc::peer_connection::configuration::media_engine::{
    MIME_TYPE_OPUS, MIME_TYPE_VP8, MediaEngine,
};
use rtc::peer_connection::sdp::RTCSessionDescription;
use rtc::peer_connection::transport::{RTCIceCandidateType, RTCIceServer};
use rtc::rtp::codec::vp8::Vp8Packet;
use rtc::rtp::packetizer::Depacketizer;
use rtc::rtp_transceiver::rtp_sender::{
    RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters, RTCRtpEncodingParameters,
    RtpCodecKind,
};
use rtc::statistics::StatsSelector;
use rtc::statistics::report::RTCStatsReportEntry;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use webrtc::data_channel::{DataChannel, DataChannelEvent};
use webrtc::media_stream::Track;
use webrtc::media_stream::track_local::TrackLocal;
use webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample;
use webrtc::media_stream::track_remote::{TrackRemote, TrackRemoteEvent};
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCIceGatheringState,
    RTCPeerConnectionState, Registry,
};
use webrtc::rtp_transceiver::RtpSender;

pub const OPUS_PAYLOAD_TYPE: u8 = 111;
pub const VP8_PAYLOAD_TYPE: u8 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PeerLinkStats {
    pub rtt_ms: Option<u32>,
    pub relayed: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum VoiceError {
    #[error("audio: {0}")]
    Audio(#[from] AudioError),
    #[error("video: {0}")]
    Video(#[from] VideoError),
    #[error("{0}")]
    Message(String),
}

impl From<webrtc::error::Error> for VoiceError {
    fn from(error: webrtc::error::Error) -> Self {
        Self::Message(error.to_string())
    }
}

pub fn opus_codec() -> RTCRtpCodecParameters {
    RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: MIME_TYPE_OPUS.to_owned(),
            clock_rate: 48_000,
            channels: 1,
            sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
            rtcp_feedback: Vec::new(),
        },
        payload_type: OPUS_PAYLOAD_TYPE,
    }
}

/// Converts the server's TURN REST credentials into WebRTC ICE servers.
pub fn turn_ice_servers(turn: &rust_chat_protocol::payloads::TurnCredentials) -> Vec<RTCIceServer> {
    vec![RTCIceServer {
        urls: turn.urls.clone(),
        username: turn.username.clone(),
        credential: turn.credential.clone(),
    }]
}

async fn pump_control(channel: Arc<dyn DataChannel>, messages: mpsc::UnboundedSender<String>) {
    while let Some(event) = channel.poll().await {
        if let DataChannelEvent::OnMessage(message) = event
            && let Ok(text) = String::from_utf8(message.data.to_vec())
        {
            let _ = messages.send(text);
        }
    }
}

async fn pump_files(channel: Arc<dyn DataChannel>, frames: mpsc::UnboundedSender<Vec<u8>>) {
    while let Some(event) = channel.poll().await {
        if let DataChannelEvent::OnMessage(message) = event {
            let _ = frames.send(message.data.to_vec());
        }
    }
}

fn is_relay(entry: Option<&RTCStatsReportEntry>) -> bool {
    matches!(
        entry,
        Some(RTCStatsReportEntry::LocalCandidate(candidate))
            | Some(RTCStatsReportEntry::RemoteCandidate(candidate))
            if candidate.candidate_type == RTCIceCandidateType::Relay
    )
}

pub fn vp8_codec() -> RTCRtpCodecParameters {
    RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: MIME_TYPE_VP8.to_owned(),
            clock_rate: 90_000,
            channels: 0,
            sdp_fmtp_line: String::new(),
            rtcp_feedback: Vec::new(),
        },
        payload_type: VP8_PAYLOAD_TYPE,
    }
}

struct RtpFrame {
    sequence: u16,
    payload: Vec<u8>,
}

#[derive(Clone)]
struct Handler {
    gathered: mpsc::UnboundedSender<()>,
    connected: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    frames: mpsc::UnboundedSender<RtpFrame>,
    video: mpsc::UnboundedSender<DecodedVideo>,
    video_lost: mpsc::UnboundedSender<()>,
    control_in: mpsc::UnboundedSender<String>,
    control: Arc<std::sync::Mutex<Option<Arc<dyn DataChannel>>>>,
    files_in: mpsc::UnboundedSender<Vec<u8>>,
    files: Arc<std::sync::Mutex<Option<Arc<dyn DataChannel>>>>,
    runtime: tokio::runtime::Handle,
}

#[async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            let _ = self.gathered.send(());
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        match state {
            RTCPeerConnectionState::Connected => {
                self.failed.store(false, Ordering::Release);
                self.connected.store(true, Ordering::Release);
            }
            RTCPeerConnectionState::Failed
            | RTCPeerConnectionState::Disconnected
            | RTCPeerConnectionState::Closed => {
                self.connected.store(false, Ordering::Release);
                self.failed.store(true, Ordering::Release);
            }
            _ => {}
        }
    }

    async fn on_data_channel(&self, channel: Arc<dyn DataChannel>) {
        let Ok(label) = channel.label().await else {
            return;
        };
        match label.as_str() {
            "control" => {
                if let Ok(mut slot) = self.control.lock() {
                    *slot = Some(channel.clone());
                }
                let messages = self.control_in.clone();
                self.runtime.spawn(pump_control(channel, messages));
            }
            "files" => {
                if let Ok(mut slot) = self.files.lock() {
                    *slot = Some(channel.clone());
                }
                let frames = self.files_in.clone();
                self.runtime.spawn(pump_files(channel, frames));
            }
            _ => {}
        }
    }

    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        if track.kind().await == RtpCodecKind::Video {
            let video = self.video.clone();
            let lost = self.video_lost.clone();
            self.runtime.spawn(async move {
                let mut decoder = VideoDecoder::new();
                let mut depacketizer = Vp8Packet::default();
                let mut frame = BytesMut::new();
                while let Some(event) = track.poll().await {
                    if let TrackRemoteEvent::OnRtpPacket(packet) = event {
                        let Ok(payload) = depacketizer.depacketize(&packet.payload) else {
                            frame.clear();
                            continue;
                        };
                        frame.extend_from_slice(&payload);
                        if packet.header.marker {
                            match decoder.decode(&frame.split().freeze()) {
                                Ok(Some(picture)) => {
                                    let _ = video.send(picture);
                                }
                                Ok(None) => {}
                                Err(_) => {
                                    let _ = lost.send(());
                                }
                            }
                        }
                    }
                }
            });
            return;
        }
        let frames = self.frames.clone();
        self.runtime.spawn(async move {
            while let Some(event) = track.poll().await {
                if let TrackRemoteEvent::OnRtpPacket(packet) = event {
                    let _ = frames.send(RtpFrame {
                        sequence: packet.header.sequence_number,
                        payload: packet.payload.to_vec(),
                    });
                }
            }
        });
    }
}

/// One peer connection carrying a bidirectional Opus track and a VP8 screen track.
pub struct VoicePeer {
    connection: Arc<dyn PeerConnection>,
    track: Arc<TrackLocalStaticSample>,
    sender: Arc<dyn RtpSender>,
    video_track: Arc<TrackLocalStaticSample>,
    video_sender: Arc<dyn RtpSender>,
    encoder: OpusEncoder,
    gathered: mpsc::UnboundedReceiver<()>,
    connected: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    /// Decoded 48 kHz mono frames from the remote peer.
    pub received: Option<mpsc::UnboundedReceiver<Vec<i16>>>,
    /// Decoded VP8 screen frames from the remote peer.
    pub received_video: Option<mpsc::UnboundedReceiver<DecodedVideo>>,
    video_lost: Option<mpsc::UnboundedReceiver<()>>,
    control: Arc<std::sync::Mutex<Option<Arc<dyn DataChannel>>>>,
    control_in: Option<mpsc::UnboundedReceiver<String>>,
    control_in_tx: mpsc::UnboundedSender<String>,
    files: Arc<std::sync::Mutex<Option<Arc<dyn DataChannel>>>>,
    files_in: Option<mpsc::UnboundedReceiver<Vec<u8>>>,
    files_in_tx: mpsc::UnboundedSender<Vec<u8>>,
    configured_ssrc: u32,
    ssrc: u32,
    payload_type: u8,
    video_ssrc: u32,
    video_payload_type: u8,
}

impl VoicePeer {
    pub async fn new(udp_addr: &str, ice_servers: Vec<RTCIceServer>) -> Result<Self, VoiceError> {
        let runtime = webrtc::runtime::default_runtime()
            .ok_or_else(|| VoiceError::Message("no WebRTC runtime".into()))?;
        let mut media = MediaEngine::default();
        media.register_codec(opus_codec(), RtpCodecKind::Audio)?;
        media.register_codec(vp8_codec(), RtpCodecKind::Video)?;
        let registry = register_default_interceptors(Registry::new(), &mut media)?;

        let (gathered_tx, gathered_rx) = mpsc::unbounded_channel();
        let (frame_tx, mut frame_rx) = mpsc::unbounded_channel::<RtpFrame>();
        let (video_tx, video_rx) = mpsc::unbounded_channel();
        let (lost_tx, lost_rx) = mpsc::unbounded_channel();
        let (control_in_tx, control_in_rx) = mpsc::unbounded_channel();
        let control_channel = Arc::new(std::sync::Mutex::new(None));
        let (files_in_tx, files_in_rx) = mpsc::unbounded_channel();
        let files_channel = Arc::new(std::sync::Mutex::new(None));
        let connected = Arc::new(AtomicBool::new(false));
        let failed = Arc::new(AtomicBool::new(false));
        let (pcm_tx, pcm_rx) = mpsc::unbounded_channel();
        let handle = tokio::runtime::Handle::current();
        handle.spawn(async move {
            let Ok(mut decoder) = OpusDecoder::new() else {
                return;
            };
            let mut jitter = JitterBuffer::new();
            while let Some(frame) = frame_rx.recv().await {
                for payload in jitter.push(frame.sequence, frame.payload) {
                    if let Ok(pcm) = decoder.decode(&payload) {
                        let _ = pcm_tx.send(pcm);
                    }
                }
            }
        });

        let handler = Arc::new(Handler {
            gathered: gathered_tx,
            connected: connected.clone(),
            failed: failed.clone(),
            frames: frame_tx,
            video: video_tx,
            video_lost: lost_tx,
            control_in: control_in_tx.clone(),
            control: control_channel.clone(),
            files_in: files_in_tx.clone(),
            files: files_channel.clone(),
            runtime: tokio::runtime::Handle::current(),
        });
        let configuration = RTCConfigurationBuilder::new()
            .with_ice_servers(ice_servers)
            .build();
        let connection = PeerConnectionBuilder::new()
            .with_configuration(configuration)
            .with_media_engine(media)
            .with_interceptor_registry(registry)
            .with_handler(handler)
            .with_runtime(runtime)
            .with_udp_addrs(vec![udp_addr.to_owned()])
            .build()
            .await?;
        let connection: Arc<dyn PeerConnection> = Arc::new(connection);

        let configured_ssrc = rand::random::<u32>();
        let codec = opus_codec();
        let track = Arc::new(TrackLocalStaticSample::new(
            Instant::now(),
            MediaStreamTrack::new(
                "rust-chat-voice-stream".to_owned(),
                format!("rust-chat-voice-{configured_ssrc}"),
                "voice".to_owned(),
                RtpCodecKind::Audio,
                vec![RTCRtpEncodingParameters {
                    rtp_coding_parameters: RTCRtpCodingParameters {
                        ssrc: Some(configured_ssrc),
                        ..Default::default()
                    },
                    codec: codec.rtp_codec.clone(),
                    ..Default::default()
                }],
            ),
        )?);
        let sender = connection
            .add_track(Arc::clone(&track) as Arc<dyn TrackLocal>)
            .await?;

        let configured_video_ssrc = rand::random::<u32>();
        let video_codec = vp8_codec();
        let video_track = Arc::new(TrackLocalStaticSample::new(
            Instant::now(),
            MediaStreamTrack::new(
                "rust-chat-screen-stream".to_owned(),
                format!("rust-chat-screen-{configured_video_ssrc}"),
                "screen".to_owned(),
                RtpCodecKind::Video,
                vec![RTCRtpEncodingParameters {
                    rtp_coding_parameters: RTCRtpCodingParameters {
                        ssrc: Some(configured_video_ssrc),
                        ..Default::default()
                    },
                    codec: video_codec.rtp_codec.clone(),
                    ..Default::default()
                }],
            ),
        )?);
        let video_sender = connection
            .add_track(Arc::clone(&video_track) as Arc<dyn TrackLocal>)
            .await?;

        Ok(Self {
            connection,
            track,
            sender,
            video_track,
            video_sender,
            encoder: OpusEncoder::new()?,
            gathered: gathered_rx,
            connected,
            failed,
            received: Some(pcm_rx),
            received_video: Some(video_rx),
            video_lost: Some(lost_rx),
            control: control_channel,
            control_in: Some(control_in_rx),
            control_in_tx,
            files: files_channel,
            files_in: Some(files_in_rx),
            files_in_tx,
            configured_ssrc,
            ssrc: configured_ssrc,
            payload_type: OPUS_PAYLOAD_TYPE,
            video_ssrc: configured_video_ssrc,
            video_payload_type: VP8_PAYLOAD_TYPE,
        })
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    /// True once the transport has failed or closed; the caller rebuilds the peer.
    pub fn is_failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }

    /// Selected candidate pair: round-trip time and whether the path is relayed.
    pub async fn link_stats(&self) -> PeerLinkStats {
        let report = self
            .connection
            .get_stats(Instant::now(), StatsSelector::None)
            .await;
        let Some(pair) = report
            .candidate_pairs()
            .find(|pair| pair.nominated)
            .or_else(|| report.candidate_pairs().next())
        else {
            return PeerLinkStats::default();
        };
        let rtt_ms = (pair.current_round_trip_time > 0.0)
            .then(|| (pair.current_round_trip_time * 1000.0).round() as u32);
        let relayed = is_relay(report.get(&pair.local_candidate_id))
            || is_relay(report.get(&pair.remote_candidate_id));
        PeerLinkStats { rtt_ms, relayed }
    }

    /// Moves the decoded-frame receiver out so a peer task can select on it while
    /// still calling `send_pcm` on the connection.
    pub fn take_received(&mut self) -> Option<mpsc::UnboundedReceiver<Vec<i16>>> {
        self.received.take()
    }

    pub fn take_received_video(&mut self) -> Option<mpsc::UnboundedReceiver<DecodedVideo>> {
        self.received_video.take()
    }

    /// Signals that the VP8 decoder lost its reference and needs a keyframe.
    pub fn take_video_lost(&mut self) -> Option<mpsc::UnboundedReceiver<()>> {
        self.video_lost.take()
    }

    /// Offerer side: creates the reliable `control` channel before the offer.
    pub async fn create_control(&mut self) -> Result<(), VoiceError> {
        if self
            .control
            .lock()
            .map(|slot| slot.is_some())
            .unwrap_or(false)
        {
            return Ok(());
        }
        let channel = self.connection.create_data_channel("control", None).await?;
        if let Ok(mut slot) = self.control.lock() {
            *slot = Some(channel.clone());
        }
        let messages = self.control_in_tx.clone();
        tokio::spawn(pump_control(channel, messages));
        Ok(())
    }

    pub fn take_control(&mut self) -> Option<mpsc::UnboundedReceiver<String>> {
        self.control_in.take()
    }

    /// Offerer side: creates the reliable `files` channel before the offer.
    pub async fn create_files(&mut self) -> Result<(), VoiceError> {
        if self
            .files
            .lock()
            .map(|slot| slot.is_some())
            .unwrap_or(false)
        {
            return Ok(());
        }
        let channel = self.connection.create_data_channel("files", None).await?;
        if let Ok(mut slot) = self.files.lock() {
            *slot = Some(channel.clone());
        }
        let frames = self.files_in_tx.clone();
        tokio::spawn(pump_files(channel, frames));
        Ok(())
    }

    pub fn take_files(&mut self) -> Option<mpsc::UnboundedReceiver<Vec<u8>>> {
        self.files_in.take()
    }

    /// Sends one binary file frame; false when the channel is not open yet.
    pub async fn send_file_frame(&self, data: Vec<u8>) -> bool {
        let channel = self.files.lock().ok().and_then(|slot| slot.clone());
        match channel {
            Some(channel) => channel
                .send(bytes::BytesMut::from(data.as_slice()))
                .await
                .is_ok(),
            None => false,
        }
    }

    pub async fn send_control(&self, text: &str) -> bool {
        let channel = self.control.lock().ok().and_then(|slot| slot.clone());
        match channel {
            Some(channel) => channel.send_text(text).await.is_ok(),
            None => false,
        }
    }

    pub async fn wait_connected(&self, timeout: Duration) -> Result<(), VoiceError> {
        let deadline = tokio::time::Instant::now() + timeout;
        while tokio::time::Instant::now() < deadline {
            if self.is_connected() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Err(VoiceError::Message("peer connection timed out".into()))
    }

    /// Builds the offer, waits for host candidates, and returns the SDP.
    pub async fn create_offer(&mut self) -> Result<String, VoiceError> {
        let offer = self.connection.create_offer(None).await?;
        self.connection.set_local_description(offer).await?;
        self.wait_gathered().await?;
        let description = self
            .connection
            .local_description()
            .await
            .ok_or_else(|| VoiceError::Message("no local description".into()))?;
        Ok(description.sdp)
    }

    /// Applies a remote offer and returns the answer SDP.
    pub async fn accept_offer(&mut self, sdp: &str) -> Result<String, VoiceError> {
        self.connection
            .set_remote_description(RTCSessionDescription::offer(sdp.to_owned())?)
            .await?;
        let answer = self.connection.create_answer(None).await?;
        self.connection.set_local_description(answer).await?;
        self.wait_gathered().await?;
        let description = self
            .connection
            .local_description()
            .await
            .ok_or_else(|| VoiceError::Message("no local description".into()))?;
        Ok(description.sdp)
    }

    pub async fn accept_answer(&mut self, sdp: &str) -> Result<(), VoiceError> {
        self.connection
            .set_remote_description(RTCSessionDescription::answer(sdp.to_owned())?)
            .await?;
        Ok(())
    }

    /// Reads the negotiated SSRC and payload type. Call once the connection is up,
    /// before the first `send_pcm` / `send_video`.
    pub async fn prepare(&mut self) -> Result<(), VoiceError> {
        self.ssrc = self
            .track
            .ssrcs()
            .await
            .first()
            .copied()
            .unwrap_or(self.configured_ssrc);
        let parameters = self.sender.get_parameters().await?;
        if let Some(codec) = parameters.rtp_parameters.codecs.first() {
            self.payload_type = codec.payload_type;
        }
        self.video_ssrc = self
            .video_track
            .ssrcs()
            .await
            .first()
            .copied()
            .unwrap_or(self.video_ssrc);
        let parameters = self.video_sender.get_parameters().await?;
        if let Some(codec) = parameters.rtp_parameters.codecs.first() {
            self.video_payload_type = codec.payload_type;
        }
        Ok(())
    }

    /// Encodes one 20 ms mono frame and writes it to the track.
    pub async fn send_pcm(&mut self, pcm: &[i16]) -> Result<(), VoiceError> {
        if pcm.len() != FRAME_SAMPLES {
            return Err(VoiceError::Message(format!(
                "expected {FRAME_SAMPLES} samples, got {}",
                pcm.len()
            )));
        }
        let packet = self.encoder.encode(pcm)?;
        let sample = Sample {
            data: bytes::Bytes::copy_from_slice(&packet),
            duration: Duration::from_millis(u64::from(FRAME_MS)),
            ..Sample::new(Instant::now())
        };
        self.track
            .sample_writer(self.ssrc, self.payload_type)
            .write_sample(&sample)
            .await?;
        Ok(())
    }

    /// Writes one pre-encoded VP8 frame (the caller encodes once for all peers).
    pub async fn send_video(&mut self, frame: &[u8], duration: Duration) -> Result<(), VoiceError> {
        let sample = Sample {
            data: bytes::Bytes::copy_from_slice(frame),
            duration,
            ..Sample::new(Instant::now())
        };
        self.video_track
            .sample_writer(self.video_ssrc, self.video_payload_type)
            .write_sample(&sample)
            .await?;
        Ok(())
    }

    pub async fn close(&self) {
        let _ = self.connection.close().await;
    }
    async fn wait_gathered(&mut self) -> Result<(), VoiceError> {
        tokio::time::timeout(Duration::from_secs(5), self.gathered.recv())
            .await
            .map_err(|_| VoiceError::Message("ICE gathering timed out".into()))?
            .ok_or_else(|| VoiceError::Message("ICE gathering channel closed".into()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_credentials_become_ice_servers() {
        let turn = rust_chat_protocol::payloads::TurnCredentials {
            urls: vec!["turn:relay.example:3478?transport=udp".into()],
            username: "1700000000:session".into(),
            credential: "secret".into(),
            ttl_seconds: 600,
        };
        let servers = turn_ice_servers(&turn);
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].urls, turn.urls);
        assert_eq!(servers[0].username, turn.username);
        assert_eq!(servers[0].credential, turn.credential);
    }
}
