use std::sync::Arc;

use futures::AsyncWriteExt;
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
