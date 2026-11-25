//! The reliable control data channel that carries remote-control messages.

use rust_chat_media::voice::VoicePeer;
use std::time::Duration;

#[tokio::test]
async fn control_channel_round_trips_both_ways() {
    let mut a = VoicePeer::new("127.0.0.1:0", Vec::new())
        .await
        .expect("peer A");
    let mut b = VoicePeer::new("127.0.0.1:0", Vec::new())
        .await
        .expect("peer B");

    a.create_control().await.expect("A control channel");
    let offer = a.create_offer().await.expect("offer");
    let answer = b.accept_offer(&offer).await.expect("answer");
    a.accept_answer(&answer).await.expect("remote answer");
    a.wait_connected(Duration::from_secs(15))
        .await
        .expect("A connects");
    b.wait_connected(Duration::from_secs(15))
        .await
        .expect("B connects");

    let mut a_rx = a.take_control().expect("A control receiver");
    let mut b_rx = b.take_control().expect("B control receiver");

    let mut delivered = false;
    for _ in 0..100 {
        if a.send_control("request").await {
            delivered = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(delivered, "A could not send on the control channel");
    let received = tokio::time::timeout(Duration::from_secs(5), b_rx.recv())
        .await
        .expect("B receives within 5 s")
        .expect("a message");
    assert_eq!(received, "request");

    let mut delivered = false;
    for _ in 0..100 {
        if b.send_control("grant").await {
            delivered = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(delivered, "B could not send on the control channel");
    let received = tokio::time::timeout(Duration::from_secs(5), a_rx.recv())
        .await
        .expect("A receives within 5 s")
        .expect("a message");
    assert_eq!(received, "grant");

    a.close().await;
    b.close().await;
}
