//! Accepts the orchestrator's streams into one queue; to any other peer no
//! protocol is offered. libp2p-stream's own `accept` has a one-slot queue per
//! protocol, open to every peer, and drops a stream that finds it full: a peer
//! opening streams made the node lose dispatches.

use std::convert::Infallible;
use std::future::{ready, Ready};
use std::task::{Context, Poll};

use libp2p::core::transport::PortUse;
use libp2p::core::upgrade::{DeniedUpgrade, InboundUpgrade, UpgradeInfo};
use libp2p::core::Endpoint;
use libp2p::swarm::handler::{ConnectionEvent, FullyNegotiatedInbound};
use libp2p::swarm::{
    ConnectionDenied, ConnectionHandler, ConnectionHandlerEvent, ConnectionId, FromSwarm,
    NetworkBehaviour, Stream, StreamProtocol, SubstreamProtocol, THandler, THandlerInEvent,
    THandlerOutEvent, ToSwarm,
};
use libp2p::{Multiaddr, PeerId};
use tokio::sync::mpsc;

const QUEUE: usize = 256;

pub struct Accepted {
    pub protocol: &'static str,
    pub stream: Stream,
}

pub struct Behaviour {
    from: PeerId,
    protocols: &'static [&'static str],
    tx: mpsc::Sender<Accepted>,
}

impl Behaviour {
    /// Accepts `protocols` from the peer `from`.
    pub fn new(
        from: PeerId,
        protocols: &'static [&'static str],
    ) -> (Self, mpsc::Receiver<Accepted>) {
        let (tx, rx) = mpsc::channel(QUEUE);
        (
            Self {
                from,
                protocols,
                tx,
            },
            rx,
        )
    }

    fn handler(&self, peer_id: PeerId) -> Handler {
        Handler {
            protocols: if peer_id == self.from {
                self.protocols
            } else {
                &[]
            },
            tx: self.tx.clone(),
        }
    }
}

impl NetworkBehaviour for Behaviour {
    type ConnectionHandler = Handler;
    type ToSwarm = Infallible;

    fn handle_established_inbound_connection(
        &mut self,
        _: ConnectionId,
        peer: PeerId,
        _: &Multiaddr,
        _: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        Ok(self.handler(peer))
    }

    fn handle_established_outbound_connection(
        &mut self,
        _: ConnectionId,
        peer: PeerId,
        _: &Multiaddr,
        _: Endpoint,
        _: PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        Ok(self.handler(peer))
    }

    fn on_swarm_event(&mut self, _: FromSwarm) {}

    fn on_connection_handler_event(
        &mut self,
        _: PeerId,
        _: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        match event {}
    }

    fn poll(&mut self, _: &mut Context<'_>) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        Poll::Pending
    }
}

pub struct Handler {
    protocols: &'static [&'static str],
    tx: mpsc::Sender<Accepted>,
}

impl ConnectionHandler for Handler {
    type FromBehaviour = Infallible;
    type ToBehaviour = Infallible;
    type InboundProtocol = Accept;
    type OutboundProtocol = DeniedUpgrade;
    type InboundOpenInfo = ();
    type OutboundOpenInfo = ();

    fn listen_protocol(&self) -> SubstreamProtocol<Accept> {
        SubstreamProtocol::new(Accept(self.protocols), ())
    }

    fn poll(
        &mut self,
        _: &mut Context<'_>,
    ) -> Poll<ConnectionHandlerEvent<DeniedUpgrade, (), Infallible>> {
        Poll::Pending
    }

    fn on_behaviour_event(&mut self, event: Infallible) {
        match event {}
    }

    fn on_connection_event(&mut self, event: ConnectionEvent<Accept, DeniedUpgrade, (), ()>) {
        if let ConnectionEvent::FullyNegotiatedInbound(FullyNegotiatedInbound {
            protocol: (stream, protocol),
            ..
        }) = event
        {
            if self.tx.try_send(Accepted { protocol, stream }).is_err() {
                tracing::warn!("[P2P] dropping inbound {protocol}: accept queue full");
            }
        }
    }
}

#[derive(Clone)]
pub struct Accept(&'static [&'static str]);

impl UpgradeInfo for Accept {
    type Info = StreamProtocol;
    type InfoIter = Vec<StreamProtocol>;

    fn protocol_info(&self) -> Self::InfoIter {
        self.0.iter().map(|p| StreamProtocol::new(p)).collect()
    }
}

impl InboundUpgrade<Stream> for Accept {
    type Output = (Stream, &'static str);
    type Error = Infallible;
    type Future = Ready<Result<Self::Output, Self::Error>>;

    fn upgrade_inbound(self, stream: Stream, info: StreamProtocol) -> Self::Future {
        let protocol = self
            .0
            .iter()
            .find(|p| **p == info.as_ref())
            .expect("negotiated protocol was offered");
        ready(Ok((stream, protocol)))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures::{AsyncReadExt, AsyncWriteExt, StreamExt};
    use libp2p::swarm::{Swarm, SwarmEvent};
    use libp2p::{identity, noise, tcp, yamux, SwarmBuilder};

    use super::*;

    const PROTOCOL: &str = "/wqc/test/1.0.0";

    fn swarm<B: NetworkBehaviour>(key: identity::Keypair, behaviour: B) -> Swarm<B> {
        SwarmBuilder::with_existing_identity(key)
            .with_tokio()
            .with_tcp(
                tcp::Config::default(),
                noise::Config::new,
                yamux::Config::default,
            )
            .unwrap()
            .with_behaviour(|_| behaviour)
            .unwrap()
            .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(Duration::from_secs(60)))
            .build()
    }

    /// Connects a peer to `to` and returns its stream control.
    async fn connect(
        key: identity::Keypair,
        to: PeerId,
        addr: &Multiaddr,
    ) -> libp2p_stream::Control {
        let mut peer = swarm(key, libp2p_stream::Behaviour::new());
        peer.dial(addr.clone().with_p2p(to).unwrap()).unwrap();
        while !matches!(
            peer.select_next_some().await,
            SwarmEvent::ConnectionEstablished { .. }
        ) {}
        let control = peer.behaviour().new_control();
        tokio::spawn(async move {
            loop {
                peer.select_next_some().await;
            }
        });
        control
    }

    #[tokio::test]
    async fn accepts_only_the_given_peer_and_queues_a_burst() {
        const BURST: usize = 64;
        let protocol = StreamProtocol::new(PROTOCOL);
        let orchestrator = identity::Keypair::generate_ed25519();
        let (behaviour, mut accepted) =
            Behaviour::new(orchestrator.public().to_peer_id(), &[PROTOCOL]);
        let mut node = swarm(identity::Keypair::generate_ed25519(), behaviour);
        let node_id = *node.local_peer_id();
        node.listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .unwrap();
        let addr = loop {
            if let SwarmEvent::NewListenAddr { address, .. } = node.select_next_some().await {
                break address;
            }
        };
        tokio::spawn(async move {
            loop {
                node.select_next_some().await;
            }
        });

        let mut other = connect(identity::Keypair::generate_ed25519(), node_id, &addr).await;
        assert!(matches!(
            other.open_stream(node_id, protocol.clone()).await,
            Err(libp2p_stream::OpenStreamError::UnsupportedProtocol(_))
        ));

        // Nothing reads the queue while the burst comes in.
        let mut control = connect(orchestrator, node_id, &addr).await;
        for i in 0..BURST {
            let mut stream = control
                .open_stream(node_id, protocol.clone())
                .await
                .unwrap();
            stream.write_all(&[i as u8]).await.unwrap();
            stream.close().await.unwrap();
        }
        for _ in 0..BURST {
            let mut got = tokio::time::timeout(Duration::from_secs(5), accepted.recv())
                .await
                .expect("a stream of the burst was dropped")
                .unwrap();
            assert_eq!(got.protocol, PROTOCOL);
            let mut body = Vec::new();
            got.stream.read_to_end(&mut body).await.unwrap();
            assert_eq!(body.len(), 1);
        }
        assert!(accepted.try_recv().is_err());
    }
}
