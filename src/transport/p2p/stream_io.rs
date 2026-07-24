use std::sync::Arc;

use futures::{AsyncReadExt, AsyncWriteExt};
use libp2p::PeerId;
use libp2p::StreamProtocol;
use libp2p_stream::Control;
use tokio::sync::Mutex;

pub async fn write_outbound_stream(
    control: &Arc<Mutex<Control>>,
    peer_id: PeerId,
    protocol: &'static str,
    payload: &[u8],
) -> anyhow::Result<()> {
    let stream_protocol = StreamProtocol::new(protocol);
    let mut control = control.lock().await;
    let mut stream = control
        .open_stream(peer_id, stream_protocol)
        .await
        .map_err(|e| anyhow::anyhow!("failed to open stream to {peer_id} on {protocol}: {e}"))?;

    stream.write_all(payload).await?;
    stream.close().await?;
    Ok(())
}

/// Write payload, half-close the write side, then wait for a JSON ack from the peer.
///
/// Expected ack: `{"ok":true}` or `{"ok":false,"error":"..."}`.
pub async fn write_outbound_stream_expect_ack(
    control: &Arc<Mutex<Control>>,
    peer_id: PeerId,
    protocol: &'static str,
    payload: &[u8],
) -> anyhow::Result<()> {
    let stream_protocol = StreamProtocol::new(protocol);
    let mut control = control.lock().await;
    let mut stream = control
        .open_stream(peer_id, stream_protocol)
        .await
        .map_err(|e| anyhow::anyhow!("failed to open stream to {peer_id} on {protocol}: {e}"))?;

    stream.write_all(payload).await?;
    AsyncWriteExt::close(&mut stream).await?;

    let mut resp = Vec::new();
    AsyncReadExt::read_to_end(&mut stream, &mut resp)
        .await
        .map_err(|e| anyhow::anyhow!("failed to read ack on {protocol}: {e}"))?;

    let ack: serde_json::Value = serde_json::from_slice(&resp).map_err(|e| {
        anyhow::anyhow!(
            "invalid ack on {protocol}: {e}; body={}",
            String::from_utf8_lossy(&resp)
        )
    })?;
    if ack.get("ok").and_then(|v| v.as_bool()) == Some(true) {
        return Ok(());
    }
    let err = ack
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("orchestrator rejected stream");
    anyhow::bail!("{protocol} rejected: {err}")
}
