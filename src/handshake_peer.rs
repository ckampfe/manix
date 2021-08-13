use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tracing::{info, instrument};

use crate::messages;
use crate::peer::Peer;
use crate::peer::PeerOptions;
use crate::{peer, peer_protocol, InfoHash, PeerId};

#[derive(Debug)]
pub(crate) struct HandshakePeerOptions {
    pub(crate) socket: TcpStream,
    pub(crate) peer_id: PeerId,
    pub(crate) info_hash: InfoHash,
    pub(crate) peer_to_torrent_tx: tokio::sync::mpsc::Sender<messages::PeerToTorrent>,
    pub(crate) global_permit: tokio::sync::OwnedSemaphorePermit,
    pub(crate) torrent_permit: tokio::sync::OwnedSemaphorePermit,
    pub(crate) piece_length: usize,
    pub(crate) chunk_length: usize,
}

pub(crate) struct HandshakePeer {
    connection: tokio_util::codec::Framed<tokio::net::TcpStream, peer_protocol::HandshakeCodec>,
    peer_id: PeerId,
    info_hash: InfoHash,
    peer_to_torrent_tx: tokio::sync::mpsc::Sender<messages::PeerToTorrent>,
    global_permit: tokio::sync::OwnedSemaphorePermit,
    torrent_permit: tokio::sync::OwnedSemaphorePermit,
    piece_length: usize,
    chunk_length: usize,
}

impl HandshakePeer {
    pub(crate) fn new(options: HandshakePeerOptions) -> Self {
        let connection =
            tokio_util::codec::Framed::new(options.socket, peer_protocol::HandshakeCodec);

        Self {
            connection,
            peer_id: options.peer_id,
            info_hash: options.info_hash,
            peer_to_torrent_tx: options.peer_to_torrent_tx,
            global_permit: options.global_permit,
            torrent_permit: options.torrent_permit,
            piece_length: options.piece_length,
            chunk_length: options.chunk_length,
        }
    }

    #[instrument(skip(self))]
    pub(crate) async fn handshake_remote_peer(mut self) -> Result<Peer, std::io::Error> {
        self.send_handshake().await?;
        info!("HANDSHAKE SENT");

        let handshake = self.receive_handshake().await;
        info!("HANDSHAKE RECEIVED");

        if let Some(message) = handshake
        // if the info hashes match, we can proceed
        // if not, sever the connection and drop the semaphore permit
        {
            let peer_protocol::Handshake {
                peer_id: remote_peer_id,
                info_hash: remote_info_hash,
                ..
            } = message?;
            if self.info_hash == remote_info_hash {
                info!("HANDSHAKE WAS GOOD");

                let torrent_to_peer_rx = self.register_with_owning_torrent(remote_peer_id).await?;

                let connection =
                    peer_protocol::set_codec(self.connection, peer_protocol::MessageCodec);

                let peer_options = PeerOptions {
                    connection,
                    peer_id: self.peer_id,
                    remote_peer_id,
                    info_hash: self.info_hash,
                    peer_to_torrent_tx: self.peer_to_torrent_tx,
                    torrent_to_peer_rx,
                    global_permit: self.global_permit,
                    torrent_permit: self.torrent_permit,
                    piece_length: self.piece_length,
                    chunk_length: self.chunk_length,
                    choke_state: peer::ChokeState::Choked,
                    interest_state: peer::InterestState::NotInterested,
                };

                let peer = Peer::new(peer_options);

                info!("transitioning from HandshakePeer to Peer");

                Ok(peer)
            } else {
                info!("HANDSHAKE WAS BAD1");
                Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "remote info_hash did not match local info hash",
                ))
            }
        } else {
            todo!()
        }

        // otherwise if the message is NOT a handshake, it is invalid,
        // so drop the permit and the connection
    }

    #[instrument(skip(self))]
    async fn send_handshake(&mut self) -> Result<(), std::io::Error> {
        let peer_id = self.get_peer_id_machine_readable();
        let info_hash = self.get_info_hash();

        self.connection
            .send(peer_protocol::Handshake {
                protocol_extension_bytes: peer_protocol::PROTOCOL_EXTENSION_HEADER,
                peer_id,
                info_hash,
            })
            .await?;

        self.connection.flush().await?;

        Ok(())
    }

    async fn receive_handshake(
        &mut self,
    ) -> Option<Result<peer_protocol::Handshake, std::io::Error>> {
        self.connection.next().await
    }

    fn get_peer_id_machine_readable(&self) -> PeerId {
        self.peer_id
    }

    fn get_info_hash(&self) -> InfoHash {
        self.info_hash
    }

    #[instrument(skip(self))]
    async fn register_with_owning_torrent(
        &mut self,
        remote_peer_id: PeerId,
    ) -> Result<tokio::sync::mpsc::Receiver<messages::TorrentToPeer>, std::io::Error> {
        let (torrent_to_peer_tx, torrent_to_peer_rx) = tokio::sync::mpsc::channel(32);
        self.send_to_owned_torrent(messages::PeerToTorrent::Register {
            remote_peer_id,
            torrent_to_peer_tx,
        })
        .await?;

        Ok(torrent_to_peer_rx)
    }

    #[instrument(skip(self))]
    async fn send_to_owned_torrent(
        &self,
        message: messages::PeerToTorrent,
    ) -> Result<(), std::io::Error> {
        self.peer_to_torrent_tx
            .send(message)
            .await
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::BrokenPipe, e.to_string()))
    }
}
