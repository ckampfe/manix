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
        max_peer_connections: Arc<tokio::sync::Semaphore>,
        peer_to_torrent_tx: tokio::sync::mpsc::Sender<PeerToTorrent>,
    ) -> Result<Self, std::io::Error> {
        // TODO make this configurable
        let handle = tokio::spawn(async move {
            let listener = TcpListener::bind(format!("127.0.0.1:{}", port)).await?;

            loop {
                let max_peer_connections = max_peer_connections.clone();
                let peer_to_torrent_tx = peer_to_torrent_tx.clone();

                let permit_fut = max_peer_connections
                    .acquire_owned()
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::NotFound, e.to_string()));

                let (permit, (socket, _socket_addr)) = try_join!(permit_fut, listener.accept())?;

                tokio::spawn(async move {
                    let peer = Peer::new(socket, peer_id, info_hash, peer_to_torrent_tx).await;
                    let peer_event_loop_task = Peer::enter_event_loop(peer, permit);
                });
            }
        });

        Ok(Self { port, handle })
    }
}
