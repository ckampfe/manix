use crate::messages;
use crate::peer::Peer;
use crate::{InfoHash, PeerId};
use futures_util::future::TryFutureExt;
use std::sync::Arc;
use tokio::net::{TcpListener, ToSocketAddrs};
use tokio::try_join;
use tracing::{debug, instrument};

#[derive(Debug)]
pub(crate) struct Listener<A: ToSocketAddrs> {
    address: A,
    peer_id: PeerId,
    info_hash: InfoHash,
    global_max_peer_connections: Arc<tokio::sync::Semaphore>,
    torrent_max_peer_connections: Arc<tokio::sync::Semaphore>,
    peer_to_torrent_tx: tokio::sync::mpsc::Sender<messages::PeerToTorrent>,
    listener: Option<TcpListener>,
}

impl<A: ToSocketAddrs + Clone + std::fmt::Debug> Listener<A> {
    pub(crate) fn new(
        address: A,
        peer_id: PeerId,
        info_hash: InfoHash,
        global_max_peer_connections: Arc<tokio::sync::Semaphore>,
        torrent_max_peer_connections: Arc<tokio::sync::Semaphore>,
        peer_to_torrent_tx: tokio::sync::mpsc::Sender<messages::PeerToTorrent>,
    ) -> Self
    where
        A: ToSocketAddrs,
    {
        Self {
            address,
            peer_id,
            info_hash,
            global_max_peer_connections,
            torrent_max_peer_connections,
            peer_to_torrent_tx,
            listener: None,
        }
    }

    #[instrument(skip(self))]
    pub(crate) async fn listen(&mut self) -> Result<(), std::io::Error> {
        let listener = TcpListener::bind(self.address.clone()).await?;
        self.listener = Some(listener);
        Ok(())
    }

    #[instrument(skip(self))]
    pub(crate) async fn accept(&self) -> Result<Peer, std::io::Error> {
        let global_max_peer_connections = self.global_max_peer_connections.clone();
        let torrent_max_peer_connections = self.torrent_max_peer_connections.clone();
        let peer_to_torrent_tx = self.peer_to_torrent_tx.clone();

        let available_global = global_max_peer_connections.available_permits();
        let available_torrent = torrent_max_peer_connections.available_permits();
        debug!("available global: {}", available_global);
        debug!("available per torrent: {}", available_torrent);

        debug!("1");

        let global_max_peers_permit_fut = global_max_peer_connections
            .acquire_owned()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::NotFound, e.to_string()));

        debug!("2");

        let torrent_max_peers_permit_fut = torrent_max_peer_connections
            .acquire_owned()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::NotFound, e.to_string()));

        debug!("3");

        let this_listener = self.listener.as_ref().unwrap();

        let addr = this_listener.local_addr().unwrap();

        debug!("{}", addr);

        let (global_permit, torrent_permit, (socket, _socket_addr)) = try_join!(
            global_max_peers_permit_fut,
            torrent_max_peers_permit_fut,
            this_listener.accept()
        )?;

        debug!("4");

        let peer = Peer::new(
            socket,
            self.peer_id,
            self.info_hash,
            peer_to_torrent_tx,
            global_permit,
            torrent_permit,
        );

        Ok(peer)
    }
}
