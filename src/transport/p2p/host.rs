use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use libp2p::gossipsub::{self, IdentTopic, IdentityTransform, MessageAuthenticity};
use libp2p::identify;
use libp2p::swarm::{NetworkBehaviour, Swarm, SwarmEvent};
use libp2p::{noise, tcp, yamux, Multiaddr, PeerId, StreamProtocol, SwarmBuilder};
use libp2p_stream as stream;
use tokio::sync::Mutex;
use tokio::time::Sleep;

use crate::application::state::AppState;
use crate::config::{libp2p_keypair_from_signing_key, NodeConfig};
use crate::domain::bid;
use crate::domain::p2p::{
    parse_signed_announcement, ANNOUNCEMENT_TOPIC, PROTOCOL_ANNOUNCE, PROTOCOL_DISPATCH,
};
use crate::domain::pcs::{PROTOCOL_PCS_OPEN, PROTOCOL_PCS_REQUEST};
use crate::domain::result::PROTOCOL_RESULT;
use crate::transport::p2p::announce_handler::spawn_announce_handler;
use crate::transport::p2p::bid_client::{spawn_incoming_stream_sink, BidClient};
use crate::transport::p2p::dispatch_handler::spawn_dispatch_handler;
use crate::transport::p2p::pcs_open_handler::spawn_pcs_open_handler;
use crate::transport::p2p::pcs_request_handler::spawn_pcs_request_handler;

const BOOTSTRAP_REDIAL_INITIAL: Duration = Duration::from_secs(1);
const BOOTSTRAP_REDIAL_MAX: Duration = Duration::from_secs(60);

/// Exponential backoff for orchestrator bootstrap redial (1s, 2s, 4s, … capped at 60s).
fn bootstrap_redial_delay(attempt: u32) -> Duration {
    let shift = attempt.min(16);
    let multiplier = 1u32.checked_shl(shift).unwrap_or(u32::MAX);
    BOOTSTRAP_REDIAL_INITIAL
        .saturating_mul(multiplier)
        .min(BOOTSTRAP_REDIAL_MAX)
}

fn parse_bootstrap_addrs(peers: &[String]) -> Vec<Multiaddr> {
    peers
        .iter()
        .filter_map(|peer| match peer.parse::<Multiaddr>() {
            Ok(addr) => Some(addr),
            Err(e) => {
                tracing::warn!("Invalid bootstrap multiaddr '{}': {}", peer, e);
                None
            }
        })
        .collect()
}

fn dial_bootstrap_peers(swarm: &mut Swarm<NodeBehaviour>, addrs: &[Multiaddr], label: &str) {
    for addr in addrs {
        match swarm.dial(addr.clone()) {
            Ok(()) => tracing::info!("[P2P] {label} bootstrap peer {addr}"),
            Err(e) => tracing::warn!("[P2P] Failed to {label} bootstrap peer {addr}: {e}"),
        }
    }
}

/// (Re)subscribe to orchestrator announcements after a full reconnect.
///
/// rust-libp2p #1671: a local subscribe that predates the current connection often
/// never sends SUBSCRIBE to the new peer. Force unsubscribe→subscribe once per
/// reconnect cycle (tracked by `orch_gossip_live`), not once per transport —
/// dual TCP+QUIC must not unsubscribe on the second ConnectionEstablished or the
/// hub briefly loses the peer from `topic_peers`.
fn ensure_orchestrator_gossip_subscription(
    swarm: &mut Swarm<NodeBehaviour>,
    topic: &IdentTopic,
    orchestrator_peer_id: PeerId,
    orch_gossip_live: &mut bool,
) {
    swarm
        .behaviour_mut()
        .gossipsub
        .add_explicit_peer(&orchestrator_peer_id);

    if *orch_gossip_live {
        tracing::debug!(
            "[P2P Gossip] Additional orchestrator transport; keeping existing subscription"
        );
        return;
    }

    let _ = swarm.behaviour_mut().gossipsub.unsubscribe(topic);
    match swarm.behaviour_mut().gossipsub.subscribe(topic) {
        Ok(true) => {
            *orch_gossip_live = true;
            tracing::info!(
                "[P2P Gossip] Subscribed to {} after connecting to orchestrator",
                ANNOUNCEMENT_TOPIC
            );
        }
        Ok(false) => tracing::warn!(
            "[P2P Gossip] Forced subscribe to {} returned false",
            ANNOUNCEMENT_TOPIC
        ),
        Err(e) => tracing::warn!(
            "[P2P Gossip] Subscribe to {} failed: {:?}",
            ANNOUNCEMENT_TOPIC,
            e
        ),
    }
}

/// Drop local gossip interest when the orchestrator link is fully gone so the next
/// dial sends a fresh SUBSCRIBE control message (go-libp2p only tracks live subscriptions).
fn unsubscribe_orchestrator_announcements(swarm: &mut Swarm<NodeBehaviour>, topic: &IdentTopic) {
    if swarm.behaviour_mut().gossipsub.unsubscribe(topic) {
        tracing::info!(
            "[P2P Gossip] Unsubscribed from {} after orchestrator disconnect",
            ANNOUNCEMENT_TOPIC
        );
    } else {
        tracing::debug!(
            "[P2P Gossip] Already unsubscribed from {}",
            ANNOUNCEMENT_TOPIC
        );
    }
}

fn schedule_bootstrap_redial(attempt: u32, sleep: &mut Option<Pin<Box<Sleep>>>) {
    let delay = bootstrap_redial_delay(attempt);
    tracing::info!(
        "[P2P] Scheduling bootstrap redial in {:?} (attempt {})",
        delay,
        attempt
    );
    *sleep = Some(Box::pin(tokio::time::sleep(delay)));
}

#[derive(NetworkBehaviour)]
struct NodeBehaviour {
    identify: identify::Behaviour,
    gossipsub: gossipsub::Behaviour<IdentityTransform>,
    stream: stream::Behaviour,
}

pub fn spawn(config: NodeConfig, state: Arc<AppState>) {
    tokio::spawn(async move {
        if let Err(e) = run(config, state).await {
            tracing::error!("P2P host exited: {}", e);
        }
    });
}

async fn run(config: NodeConfig, state: Arc<AppState>) -> anyhow::Result<()> {
    let keypair = libp2p_keypair_from_signing_key(&config.signing_key)?;
    let local_peer_id = keypair.public().to_peer_id();
    let idle_timeout = Duration::from_secs(config.p2p_idle_timeout_secs);

    // Small star mesh: default D=6 never forms with a single bootstrap peer.
    let gossipsub_config = gossipsub::ConfigBuilder::default()
        .heartbeat_interval(Duration::from_secs(1))
        .mesh_n(1)
        .mesh_n_low(1)
        .mesh_n_high(3)
        .mesh_outbound_min(0)
        .flood_publish(true)
        .build()
        .map_err(|e| anyhow::anyhow!("gossipsub config: {}", e))?;

    let gossipsub_behaviour: gossipsub::Behaviour<IdentityTransform> = gossipsub::Behaviour::new(
        MessageAuthenticity::Signed(keypair.clone()),
        gossipsub_config,
    )
    .map_err(|e| anyhow::anyhow!("gossipsub behaviour: {}", e))?;

    let topic = IdentTopic::new(ANNOUNCEMENT_TOPIC);

    let mut swarm = SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_quic()
        .with_behaviour(|key| {
            let identify =
                identify::Behaviour::new(identify::Config::new("/wqc/1.0.0".into(), key.public()));
            NodeBehaviour {
                identify,
                gossipsub: gossipsub_behaviour,
                stream: stream::Behaviour::new(),
            }
        })?
        .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(idle_timeout))
        .build();

    let bid_protocol = StreamProtocol::new(bid::PROTOCOL_BID);
    let announce_protocol = StreamProtocol::new(PROTOCOL_ANNOUNCE);
    let dispatch_protocol = StreamProtocol::new(PROTOCOL_DISPATCH);
    let mut register_control = swarm.behaviour().stream.new_control();
    let bid_incoming = register_control
        .accept(bid_protocol)
        .map_err(|e| anyhow::anyhow!("failed to register bid stream protocol: {:?}", e))?;
    spawn_incoming_stream_sink(bid_incoming);

    let announce_incoming = register_control
        .accept(announce_protocol)
        .map_err(|e| anyhow::anyhow!("failed to register announce stream protocol: {:?}", e))?;

    let dispatch_incoming = register_control
        .accept(dispatch_protocol)
        .map_err(|e| anyhow::anyhow!("failed to register dispatch stream protocol: {:?}", e))?;

    let result_incoming = register_control
        .accept(StreamProtocol::new(PROTOCOL_RESULT))
        .map_err(|e| anyhow::anyhow!("failed to register result stream protocol: {:?}", e))?;
    spawn_incoming_stream_sink(result_incoming);

    let pcs_request_incoming = register_control
        .accept(StreamProtocol::new(PROTOCOL_PCS_REQUEST))
        .map_err(|e| anyhow::anyhow!("failed to register pcs request stream protocol: {:?}", e))?;

    let pcs_open_incoming = register_control
        .accept(StreamProtocol::new(PROTOCOL_PCS_OPEN))
        .map_err(|e| anyhow::anyhow!("failed to register pcs open stream protocol: {:?}", e))?;

    let bid_control = Arc::new(Mutex::new(swarm.behaviour().stream.new_control()));
    {
        let mut guard = state.p2p_stream_control.lock().await;
        *guard = Some(bid_control.clone());
    }

    let orchestrator_peer_id = config
        .orchestrator_peer_id
        .ok_or_else(|| anyhow::anyhow!("orchestrator peer id not configured"))?;

    spawn_announce_handler(
        announce_incoming,
        bid_control.clone(),
        config.clone(),
        state.clone(),
        orchestrator_peer_id,
    );

    spawn_dispatch_handler(
        dispatch_incoming,
        state.clone(),
        config.clone(),
        orchestrator_peer_id,
    );

    spawn_pcs_request_handler(
        pcs_request_incoming,
        state.clone(),
        config.clone(),
        orchestrator_peer_id,
    );

    spawn_pcs_open_handler(
        pcs_open_incoming,
        bid_control.clone(),
        state.clone(),
        config.clone(),
        orchestrator_peer_id,
    );

    let tcp_addr: Multiaddr = format!("/ip4/0.0.0.0/tcp/{}", config.p2p_listen_port).parse()?;
    let quic_addr: Multiaddr =
        format!("/ip4/0.0.0.0/udp/{}/quic-v1", config.p2p_listen_port).parse()?;
    swarm.listen_on(tcp_addr)?;
    swarm.listen_on(quic_addr)?;

    let bootstrap_addrs = parse_bootstrap_addrs(&config.bootstrap_peers);
    dial_bootstrap_peers(&mut swarm, &bootstrap_addrs, "Dialing");

    let mut redial_attempt: u32 = 0;
    let mut redial_sleep: Option<Pin<Box<Sleep>>> = None;
    // Cleared on full orchestrator disconnect; set after a successful force-subscribe.
    let mut orch_gossip_live = false;
    if !swarm
        .connected_peers()
        .any(|peer_id| *peer_id == orchestrator_peer_id)
    {
        // Orchestrator may still be starting (e.g. air rebuild). Retry until connected.
        schedule_bootstrap_redial(redial_attempt, &mut redial_sleep);
    }

    tracing::info!(
        "P2P host started (peer_id={}, listen_port={})",
        local_peer_id,
        config.p2p_listen_port
    );

    loop {
        tokio::select! {
            event = swarm.select_next_some() => {
                handle_swarm_event(
                    event,
                    &mut swarm,
                    &topic,
                    orchestrator_peer_id,
                    &bid_control,
                    &config,
                    &state,
                    &mut redial_attempt,
                    &mut redial_sleep,
                    &mut orch_gossip_live,
                );
            }
            _ = async {
                match &mut redial_sleep {
                    Some(sleep) => sleep.as_mut().await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                redial_attempt = redial_attempt.saturating_add(1);
                dial_bootstrap_peers(&mut swarm, &bootstrap_addrs, "Redialing");
                redial_sleep = None;
                if !swarm
                    .connected_peers()
                    .any(|peer_id| *peer_id == orchestrator_peer_id)
                {
                    schedule_bootstrap_redial(redial_attempt, &mut redial_sleep);
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_swarm_event(
    event: SwarmEvent<NodeBehaviourEvent>,
    swarm: &mut Swarm<NodeBehaviour>,
    topic: &IdentTopic,
    orchestrator_peer_id: PeerId,
    bid_control: &Arc<Mutex<stream::Control>>,
    config: &NodeConfig,
    state: &Arc<AppState>,
    redial_attempt: &mut u32,
    redial_sleep: &mut Option<Pin<Box<Sleep>>>,
    orch_gossip_live: &mut bool,
) {
    match event {
        SwarmEvent::Behaviour(NodeBehaviourEvent::Gossipsub(gossipsub::Event::Subscribed {
            peer_id,
            topic: subscribed_topic,
        })) => {
            tracing::info!(
                "[P2P Gossip] Peer {} subscribed to {}",
                peer_id,
                subscribed_topic
            );
        }
        SwarmEvent::Behaviour(NodeBehaviourEvent::Gossipsub(gossipsub::Event::Message {
            message,
            ..
        })) => {
            let Some(orchestrator_pubkey) = config.orchestrator_public_key.as_deref() else {
                tracing::warn!(
                    "[P2P Gossip] Ignoring announcement on {}: orchestrator public key not configured",
                    message.topic
                );
                return;
            };

            match parse_signed_announcement(&message.data, orchestrator_pubkey) {
                Ok(announcement) => {
                    tracing::info!(
                        "[P2P Gossip] TaskAnnouncement task_id={} qubits={} difficulty={}",
                        announcement.task_id,
                        announcement.global_qubit_count,
                        announcement.bid_difficulty
                    );

                    let control = bid_control.clone();
                    let node_config = config.clone();
                    let node_state = state.clone();
                    let announcement_clone = announcement.clone();
                    tokio::spawn(async move {
                        let client = BidClient::new(control, node_config, node_state);
                        if let Err(e) = client
                            .submit_bid(announcement_clone, orchestrator_peer_id)
                            .await
                        {
                            tracing::warn!("[P2P Bid] Failed to submit bid: {}", e);
                        }
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        "[P2P Gossip] Failed to decode announcement on topic {}: {}",
                        message.topic,
                        e
                    );
                }
            }
        }
        SwarmEvent::NewListenAddr { address, .. } => {
            tracing::info!(
                "[P2P] Listening on {}/p2p/{}",
                address,
                swarm.local_peer_id()
            );
        }
        SwarmEvent::ConnectionEstablished { peer_id, .. } => {
            tracing::info!("[P2P] Connected to peer {}", peer_id);
            crate::infra::metrics::set_connected_peers(swarm.connected_peers().count());
            if peer_id == orchestrator_peer_id {
                *redial_attempt = 0;
                *redial_sleep = None;
                crate::infra::metrics::set_orchestrator_connected(true);
                ensure_orchestrator_gossip_subscription(swarm, topic, peer_id, orch_gossip_live);
            }
        }
        SwarmEvent::ConnectionClosed { peer_id, .. } => {
            tracing::info!("[P2P] Disconnected from peer {}", peer_id);
            crate::infra::metrics::set_connected_peers(swarm.connected_peers().count());
            if peer_id == orchestrator_peer_id {
                let still_connected = swarm.is_connected(&orchestrator_peer_id);
                crate::infra::metrics::set_orchestrator_connected(still_connected);
                if !still_connected {
                    unsubscribe_orchestrator_announcements(swarm, topic);
                    *orch_gossip_live = false;
                }
                schedule_bootstrap_redial(*redial_attempt, redial_sleep);
            }
        }
        SwarmEvent::OutgoingConnectionError {
            peer_id: Some(peer_id),
            error,
            ..
        } if peer_id == orchestrator_peer_id => {
            tracing::warn!(
                "[P2P] Outgoing connection to orchestrator failed: {}",
                error
            );
            schedule_bootstrap_redial(*redial_attempt, redial_sleep);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_redial_delay_is_exponential_and_capped() {
        assert_eq!(bootstrap_redial_delay(0), Duration::from_secs(1));
        assert_eq!(bootstrap_redial_delay(1), Duration::from_secs(2));
        assert_eq!(bootstrap_redial_delay(2), Duration::from_secs(4));
        assert_eq!(bootstrap_redial_delay(10), Duration::from_secs(60));
        assert_eq!(bootstrap_redial_delay(100), Duration::from_secs(60));
    }
}
