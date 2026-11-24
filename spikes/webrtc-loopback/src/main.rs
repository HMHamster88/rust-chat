//! M0 media gate: two `webrtc-rs` peer connections in one process, a data channel between them,
//! and one payload delivered end to end over loopback.
//!
//! This proves the stack compiles and connects on this machine before the M3 work adds Opus,
//! jitter buffering, VP8 and the real signaling path.

use anyhow::{Context, Result, bail};
use bytes::BytesMut;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use webrtc::data_channel::{DataChannel, DataChannelEvent};
use webrtc::peer_connection::{
    MediaEngine, PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler,
    RTCConfigurationBuilder, RTCIceGatheringState, RTCPeerConnectionState, Registry,
    register_default_interceptors,
};
use webrtc::runtime::{Runtime, default_runtime};

const PAYLOAD: &[u8] = b"hello from rust-chat over an SCTP data channel";

struct Handler {
    name: &'static str,
    gathered: mpsc::UnboundedSender<()>,
    received: Option<mpsc::UnboundedSender<Vec<u8>>>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            let _ = self.gathered.send(());
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        println!("[{}] connection state: {state:?}", self.name);
    }

    async fn on_data_channel(&self, channel: Arc<dyn DataChannel>) {
        let label = channel.label().await.unwrap_or_default();
        println!("[{}] remote opened data channel '{label}'", self.name);
        let received = self.received.clone();
        tokio::spawn(async move {
            while let Some(event) = channel.poll().await {
                match event {
                    DataChannelEvent::OnMessage(message) => {
                        if let Some(received) = received.as_ref() {
                            let _ = received.send(message.data.to_vec());
                        }
                    }
                    DataChannelEvent::OnClose => break,
                    _ => {}
                }
            }
        });
    }
}

async fn build_peer(
    name: &'static str,
    runtime: Arc<dyn Runtime>,
    handler: Arc<Handler>,
) -> Result<impl PeerConnection> {
    let mut media = MediaEngine::default();
    media.register_default_codecs()?;
    let registry = register_default_interceptors(Registry::new(), &mut media)?;
    let connection = PeerConnectionBuilder::new()
        .with_configuration(RTCConfigurationBuilder::new().build())
        .with_media_engine(media)
        .with_interceptor_registry(registry)
        .with_handler(handler)
        .with_runtime(runtime)
        .with_udp_addrs(vec!["127.0.0.1:0".to_string()])
        .build()
        .await
        .with_context(|| format!("building the {name} peer connection"))?;
    Ok(connection)
}

#[tokio::main]
async fn main() -> Result<()> {
    let runtime = default_runtime().context("no WebRTC runtime feature compiled in")?;

    let (offer_gathered_tx, mut offer_gathered_rx) = mpsc::unbounded_channel();
    let (answer_gathered_tx, mut answer_gathered_rx) = mpsc::unbounded_channel();
    let (received_tx, mut received_rx) = mpsc::unbounded_channel();

    let offerer = build_peer(
        "offerer",
        runtime.clone(),
        Arc::new(Handler {
            name: "offerer",
            gathered: offer_gathered_tx,
            received: None,
        }),
    )
    .await?;

    let answerer = build_peer(
        "answerer",
        runtime.clone(),
        Arc::new(Handler {
            name: "answerer",
            gathered: answer_gathered_tx,
            received: Some(received_tx),
        }),
    )
    .await?;

    let channel = offerer.create_data_channel("files", None).await?;
    println!("created data channel '{}'", channel.label().await?);

    // The offerer created the channel locally, so `on_data_channel` is not called for it.
    // Wait for it to open and send the gate payload.
    tokio::spawn(async move {
        while let Some(event) = channel.poll().await {
            match event {
                DataChannelEvent::OnOpen => {
                    if let Err(error) = channel.send(BytesMut::from(PAYLOAD)).await {
                        eprintln!("send failed: {error}");
                    } else {
                        println!("sent {} bytes", PAYLOAD.len());
                    }
                }
                DataChannelEvent::OnClose => break,
                _ => {}
            }
        }
    });

    // Non-trickle for the gate: exchange complete SDPs, which keeps the spike small.
    let offer = offerer.create_offer(None).await?;
    offerer.set_local_description(offer).await?;
    offer_gathered_rx
        .recv()
        .await
        .context("offerer never finished ICE gathering")?;
    let offer = offerer
        .local_description()
        .await
        .context("offerer has no local description")?;

    answerer.set_remote_description(offer).await?;
    let answer = answerer.create_answer(None).await?;
    answerer.set_local_description(answer).await?;
    answer_gathered_rx
        .recv()
        .await
        .context("answerer never finished ICE gathering")?;
    let answer = answerer
        .local_description()
        .await
        .context("answerer has no local description")?;

    offerer.set_remote_description(answer).await?;

    let received = tokio::time::timeout(Duration::from_secs(15), received_rx.recv())
        .await
        .context("timed out waiting for the data-channel payload")?
        .context("answerer channel closed before a payload arrived")?;

    if received != PAYLOAD {
        bail!(
            "payload mismatch: expected {:?}, got {:?}",
            String::from_utf8_lossy(PAYLOAD),
            String::from_utf8_lossy(&received)
        );
    }

    println!(
        "DATA-CHANNEL OK: {} bytes arrived intact",
        received.len()
    );

    offerer.close().await?;
    answerer.close().await?;
    Ok(())
}
