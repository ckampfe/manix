use std::sync::Arc;

use futures_util::future::TryFutureExt;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio::try_join;

use crate::peer::{Peer, PeerToTorrent};
use crate::{InfoHash, PeerId, Port};

pub(crate) struct Listener {
    port: Port,
    handle: JoinHandle<Result<(), std::io::Error>>,
}

impl Listener {
    pub(crate) fn new(
        port: Port,
        peer_id: PeerId,
        info_hash: InfoHash,
        global_max_peer_connections: Arc<tokio::sync::Semaphore>,
        torrent_max_peer_connections: Arc<tokio::sync::Semaphore>,
        peer_to_torrent_tx: tokio::sync::mpsc::Sender<PeerToTorrent>,
    ) -> Result<Self, std::io::Error> {
        // TODO make this listening address configurable
        let handle = tokio::spawn(async move {
            let listener = TcpListener::bind(format!("127.0.0.1:{}", port)).await?;

            loop {
                let global_max_peer_connections = global_max_peer_connections.clone();
                let torrent_max_peer_connections = torrent_max_peer_connections.clone();
                let peer_to_torrent_tx = peer_to_torrent_tx.clone();

                let global_max_peers_permit_fut = global_max_peer_connections
                    .acquire_owned()
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::NotFound, e.to_string()));

                let torrent_max_peers_permit_fut = torrent_max_peer_connections
                    .acquire_owned()
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::NotFound, e.to_string()));

                let (global_permit, torrent_permit, (socket, _socket_addr)) = try_join!(
                    global_max_peers_permit_fut,
                    torrent_max_peers_permit_fut,
                    listener.accept()
                )?;

                let peer = Peer::new(socket, peer_id, info_hash, peer_to_torrent_tx);
                let _peer_event_loop_task =
                    Peer::enter_event_loop(peer, global_permit, torrent_permit).await;
            }
        });

        Ok(Self { port, handle })
    }
}
