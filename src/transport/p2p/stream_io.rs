use std::sync::Arc;
use std::time::Duration;

use futures::{AsyncReadExt, AsyncWriteExt};
use libp2p::PeerId;
use libp2p::StreamProtocol;
use libp2p_stream::Control;
use tokio::sync::Mutex;
use tokio::time::timeout;

/// Time a send of `len` bytes may take: a base for opening the stream and an
/// ack, plus the payload at a slow uplink's rate.
#[cfg(not(test))]
const SEND_BASE: Duration = Duration::from_secs(20);
#[cfg(test)]
const SEND_BASE: Duration = Duration::from_secs(1);
#[cfg(not(test))]
const SEND_MIN_BYTES_PER_SEC: u64 = 256 << 10;
#[cfg(test)]
const SEND_MIN_BYTES_PER_SEC: u64 = 64 << 20;

fn send_limit(len: usize) -> Duration {
    SEND_BASE + Duration::from_millis(len as u64 * 1000 / SEND_MIN_BYTES_PER_SEC)
}

/// A copy of the control, so that sends do not wait for each other: holding
/// the lock while writing made a large result hold up bids.
async fn own_control(control: &Arc<Mutex<Control>>) -> Control {
    control.lock().await.clone()
}

pub async fn write_outbound_stream(
    control: &Arc<Mutex<Control>>,
    peer_id: PeerId,
    protocol: &'static str,
    payload: &[u8],
) -> anyhow::Result<()> {
    let mut control = own_control(control).await;
    let send = async {
        let mut stream = control
            .open_stream(peer_id, StreamProtocol::new(protocol))
            .await
            .map_err(|e| {
                anyhow::anyhow!("failed to open stream to {peer_id} on {protocol}: {e}")
            })?;
        stream.write_all(payload).await?;
        stream.close().await?;
        anyhow::Ok(())
    };
    timeout(send_limit(payload.len()), send)
        .await
        .map_err(|_| anyhow::anyhow!("stream to {peer_id} on {protocol} timed out"))?
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
    let mut control = own_control(control).await;
    let send = async {
        let mut stream = control
            .open_stream(peer_id, StreamProtocol::new(protocol))
            .await
            .map_err(|e| {
                anyhow::anyhow!("failed to open stream to {peer_id} on {protocol}: {e}")
            })?;
        stream.write_all(payload).await?;
        AsyncWriteExt::close(&mut stream).await?;
        let mut resp = Vec::new();
        AsyncReadExt::read_to_end(&mut stream, &mut resp)
            .await
            .map_err(|e| anyhow::anyhow!("failed to read ack on {protocol}: {e}"))?;
        anyhow::Ok(resp)
    };
    let resp = timeout(send_limit(payload.len()), send)
        .await
        .map_err(|_| anyhow::anyhow!("stream to {peer_id} on {protocol} timed out"))??;

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

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use libp2p::swarm::{Swarm, SwarmEvent};
    use libp2p::{noise, tcp, yamux, Multiaddr, SwarmBuilder};

    use super::*;

    const BIG: &str = "/wqc/test/big/1.0.0";
    const SMALL: &str = "/wqc/test/small/1.0.0";

    fn swarm() -> Swarm<libp2p_stream::Behaviour> {
        SwarmBuilder::with_new_identity()
            .with_tokio()
            .with_tcp(
                tcp::Config::default(),
                noise::Config::new,
                yamux::Config::default,
            )
            .unwrap()
            .with_behaviour(|_| libp2p_stream::Behaviour::new())
            .unwrap()
            .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(Duration::from_secs(60)))
            .build()
    }

    /// One send that the orchestrator does not read holds up only itself.
    #[tokio::test]
    async fn a_send_that_is_not_read_does_not_hold_up_another() {
        let mut orchestrator = swarm();
        let mut control = orchestrator.behaviour().new_control();
        let mut big = control.accept(StreamProtocol::new(BIG)).unwrap();
        let mut small = control.accept(StreamProtocol::new(SMALL)).unwrap();
        orchestrator
            .listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .unwrap();
        let addr: Multiaddr = loop {
            if let SwarmEvent::NewListenAddr { address, .. } = orchestrator.select_next_some().await
            {
                break address;
            }
        };
        let orchestrator_id = *orchestrator.local_peer_id();
        tokio::spawn(async move {
            loop {
                orchestrator.select_next_some().await;
            }
        });

        let mut node = swarm();
        let shared = Arc::new(Mutex::new(node.behaviour().new_control()));
        node.dial(addr.with_p2p(orchestrator_id).unwrap()).unwrap();
        while !matches!(
            node.select_next_some().await,
            SwarmEvent::ConnectionEstablished { .. }
        ) {}
        tokio::spawn(async move {
            loop {
                node.select_next_some().await;
            }
        });

        // Past the yamux window, so the write waits for a read that never comes.
        let c = shared.clone();
        let stuck = tokio::spawn(async move {
            write_outbound_stream(&c, orchestrator_id, BIG, &vec![0; 8 << 20]).await
        });
        let (_, _unread) = big.next().await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let sent = tokio::time::timeout(
            Duration::from_millis(500),
            write_outbound_stream(&shared, orchestrator_id, SMALL, b"bid"),
        )
        .await;
        sent.expect("the small send waited for the big one")
            .unwrap();
        let (_, mut got) = small.next().await.unwrap();
        let mut body = Vec::new();
        got.read_to_end(&mut body).await.unwrap();
        assert_eq!(body, b"bid");

        let ended = tokio::time::timeout(Duration::from_secs(5), stuck).await;
        assert!(ended.expect("the unread send never ends").unwrap().is_err());
    }

    #[test]
    fn a_large_send_gets_more_time() {
        assert_eq!(send_limit(0), SEND_BASE);
        assert!(send_limit(22 << 20) > send_limit(1 << 10));
    }
}
