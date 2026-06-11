use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use libp2p::gossipsub::{self, IdentTopic, IdentityTransform, MessageAuthenticity};
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{noise, tcp, yamux, Multiaddr, StreamProtocol, SwarmBuilder};
use libp2p_stream as stream;
use tokio::sync::Mutex;

use crate::application::state::AppState;
use crate::config::{libp2p_keypair_from_signing_key, NodeConfig};
use crate::domain::bid;
use crate::domain::p2p::{TaskAnnouncement, ANNOUNCEMENT_TOPIC, PROTOCOL_DISPATCH};
use crate::domain::result::PROTOCOL_RESULT;
use crate::transport::p2p::bid_client::{spawn_incoming_stream_sink, BidClient};
use crate::transport::p2p::dispatch_handler::spawn_dispatch_handler;

#[derive(NetworkBehaviour)]
struct NodeBehaviour {
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

    let gossipsub_config = gossipsub::ConfigBuilder::default()
        .heartbeat_interval(Duration::from_secs(1))
        .build()
        .map_err(|e| anyhow::anyhow!("gossipsub config: {}", e))?;

    let mut gossipsub_behaviour: gossipsub::Behaviour<IdentityTransform> = gossipsub::Behaviour::new(
        MessageAuthenticity::Signed(keypair.clone()),
        gossipsub_config,
    )
    .map_err(|e| anyhow::anyhow!("gossipsub behaviour: {}", e))?;

    let topic = IdentTopic::new(ANNOUNCEMENT_TOPIC);
    gossipsub_behaviour
        .subscribe(&topic)
        .map_err(|e| anyhow::anyhow!("gossipsub subscribe: {}", e))?;

    let mut swarm = SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_quic()
        .with_behaviour(|_| NodeBehaviour {
            gossipsub: gossipsub_behaviour,
            stream: stream::Behaviour::new(),
        })?
        .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();

    let bid_protocol = StreamProtocol::new(bid::PROTOCOL_BID);
    let dispatch_protocol = StreamProtocol::new(PROTOCOL_DISPATCH);
    let mut register_control = swarm.behaviour().stream.new_control();
    let bid_incoming = register_control
        .accept(bid_protocol)
        .map_err(|e| anyhow::anyhow!("failed to register bid stream protocol: {:?}", e))?;
    spawn_incoming_stream_sink(bid_incoming);

    let dispatch_incoming = register_control
        .accept(dispatch_protocol)
        .map_err(|e| anyhow::anyhow!("failed to register dispatch stream protocol: {:?}", e))?;

    let result_incoming = register_control
        .accept(StreamProtocol::new(PROTOCOL_RESULT))
        .map_err(|e| anyhow::anyhow!("failed to register result stream protocol: {:?}", e))?;
    spawn_incoming_stream_sink(result_incoming);

    let bid_control = Arc::new(Mutex::new(swarm.behaviour().stream.new_control()));
    {
        let mut guard = state.p2p_stream_control.lock().await;
        *guard = Some(bid_control.clone());
    }

    let orchestrator_peer_id = config
        .orchestrator_peer_id
        .ok_or_else(|| anyhow::anyhow!("WQC_ORCHESTRATOR_BOOTSTRAP must include /p2p/<peer-id>"))?;

    spawn_dispatch_handler(
        dispatch_incoming,
        state.clone(),
        config.clone(),
        orchestrator_peer_id,
    );

    let tcp_addr: Multiaddr = format!("/ip4/0.0.0.0/tcp/{}", config.p2p_listen_port).parse()?;
    let quic_addr: Multiaddr =
        format!("/ip4/0.0.0.0/udp/{}/quic-v1", config.p2p_listen_port).parse()?;

    swarm.listen_on(tcp_addr)?;
    swarm.listen_on(quic_addr)?;

    for peer in &config.bootstrap_peers {
        match peer.parse::<Multiaddr>() {
            Ok(addr) => {
                if let Err(e) = swarm.dial(addr.clone()) {
                    tracing::warn!("Failed to dial bootstrap peer {}: {}", addr, e);
                } else {
                    tracing::info!("Dialing bootstrap peer {}", addr);
                }
            }
            Err(e) => tracing::warn!("Invalid bootstrap multiaddr '{}': {}", peer, e),
        }
    }

    tracing::info!(
        "P2P host started (peer_id={}, listen_port={})",
        local_peer_id,
        config.p2p_listen_port
    );

    loop {
        match swarm.select_next_some().await {
            SwarmEvent::Behaviour(NodeBehaviourEvent::Gossipsub(
                gossipsub::Event::Message { message, .. },
            )) => {
                match serde_json::from_slice::<TaskAnnouncement>(&message.data) {
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
                tracing::info!("[P2P] Listening on {}/p2p/{}", address, local_peer_id);
            }
            SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                tracing::info!("[P2P] Connected to peer {}", peer_id);
            }
            SwarmEvent::ConnectionClosed { peer_id, .. } => {
                tracing::info!("[P2P] Disconnected from peer {}", peer_id);
            }
            _ => {}
        }
    }
}
