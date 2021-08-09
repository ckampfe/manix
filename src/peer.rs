use crate::peer_protocol::{self, HANDSHAKE_LENGTH};
use crate::{messages, Begin, Index, InfoHash, Length, PeerId};
use bitvec::{order::Lsb0, prelude::BitVec};
use std::convert::TryFrom;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{Interval, MissedTickBehavior};
use tracing::{instrument, trace};

#[derive(Debug)]
pub(crate) struct Peer {
    socket: tokio::net::TcpStream,
    peer_id: PeerId,
    remote_peer_id: Option<PeerId>,
    info_hash: InfoHash,
    peer_to_torrent_tx: tokio::sync::mpsc::Sender<messages::PeerToTorrent>,
    torrent_to_peer_rx: Option<tokio::sync::mpsc::Receiver<messages::TorrentToPeer>>,
    global_permit: tokio::sync::OwnedSemaphorePermit,
    torrent_permit: tokio::sync::OwnedSemaphorePermit,
    choke_state: ChokeState,
    interest_state: InterestState,
}

impl Peer {
    #[instrument]
    pub(crate) fn new(
        socket: TcpStream,
        peer_id: PeerId,
        info_hash: InfoHash,
        peer_to_torrent_tx: tokio::sync::mpsc::Sender<messages::PeerToTorrent>,
        global_permit: tokio::sync::OwnedSemaphorePermit,
        torrent_permit: tokio::sync::OwnedSemaphorePermit,
    ) -> Self {
        Self {
            socket,
            peer_id,
            remote_peer_id: None,
            info_hash,
            peer_to_torrent_tx,
            torrent_to_peer_rx: None,
            global_permit,
            torrent_permit,
            choke_state: ChokeState::Choked,
            interest_state: InterestState::NotInterested,
        }
    }

    #[instrument]
    pub(crate) async fn enter_event_loop(mut self) -> Result<(), std::io::Error> {
        let remote_peer_id = self.handshake_remote_peer().await?;
        self.remote_peer_id = Some(remote_peer_id);

        self.register_with_owning_torrent().await?;

        let Timers { mut keepalive } = self.start_timers().await;

        loop {
            // take ownership of the TorrentToPeer rx channel for the duration of the loop
            let mut rx = self.torrent_to_peer_rx.take().unwrap();

            tokio::select! {
                result = self.receive_message() => {
                    match result {
                        Ok(message) => {
                            match message {
                                peer_protocol::Message::Keepalive => (),
                                peer_protocol::Message::Choke => self.send_choke().await?,
                                peer_protocol::Message::Unchoke => todo!(),
                                peer_protocol::Message::Interested => todo!(),
                                peer_protocol::Message::NotInterested => todo!(),
                                peer_protocol::Message::Have { index: _ } => todo!(),
                                peer_protocol::Message::Bitfield { bitfield: _ } => todo!(),
                                peer_protocol::Message::Request { index: _, begin: _, length: _ } => todo!(),
                                peer_protocol::Message::Piece { index: _, begin: _, chunk: _ } => todo!(),
                                peer_protocol::Message::Cancel { index: _, begin: _, length: _ } => todo!(),
                                peer_protocol::Message::Handshake { protocol_extension_bytes: _, peer_id: _, info_hash: _ } => todo!(),
                            }
                        }
                        Err(e) => {
                            self.deregister_with_owning_torrent().await?;
                            return Err(e);
                        }
                    }
                }
                result = Peer::receive_message_from_owning_torrent(&mut rx) => {
                    match result {
                        Some(message) => match message {
                            messages::TorrentToPeer::Choke => {
                                self.choke_state = ChokeState::Choked;
                                self.send_choke().await?
                            },
                            messages::TorrentToPeer::NotChoked => {
                                self.choke_state = ChokeState::NotChoked;
                                self.send_unchoke().await?
                            },
                            messages::TorrentToPeer::Interested => {
                                self.interest_state = InterestState::Interested;
                                self.send_interested().await?
                            },
                            messages::TorrentToPeer::NotInterested => {
                                self.interest_state = InterestState::NotInterested;
                                self.send_not_interested().await?
                            },
                        },
                        None => todo!(),
                    }
                }
                _ = keepalive.tick() => {
                    self.send_keepalive().await?;
                }
            }

            // return ownership of the TorrentToPeer rx channel to Self
            self.torrent_to_peer_rx = Some(rx);
        }
    }

    #[instrument(skip(self))]
    async fn start_timers(&self) -> Timers {
        let mut keepalive = tokio::time::interval(std::time::Duration::from_secs(60));
        keepalive.set_missed_tick_behavior(MissedTickBehavior::Delay);

        Timers { keepalive }
    }

    #[instrument]
    async fn handshake_remote_peer(&mut self) -> Result<PeerId, std::io::Error> {
        self.send_handshake().await?;
        trace!("HANDSHAKE SENT");

        let handshake = self.receive_handshake().await?;
        trace!("HANDSHAKE RECEIVED");

        if let peer_protocol::Message::Handshake {
            peer_id: remote_peer_id,
            info_hash: remote_info_hash,
            ..
        } = handshake
        // if the info hashes match, we can proceed
        // if not, sever the connection and drop the semaphore permit
        {
            if self.info_hash == remote_info_hash {
                trace!("HANDSHAKE WAS GOOD");
                Ok(remote_peer_id)
            } else {
                trace!("HANDSHAKE WAS BAD1");
                Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "remote info_hash did not match local info hash",
                ))
            }
            // otherwise if the message is NOT a handshake, it is invalid,
            // so drop the permit and the connection
        } else {
            trace!("HANDSHAKE WAS BAD2");
            Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "received a non-handshake message when was expecting handshake",
            ))
        }
    }

    fn get_info_hash(&self) -> InfoHash {
        self.info_hash
    }

    fn get_peer_id_machine_readable(&self) -> PeerId {
        self.peer_id
    }

    // fn get_remote_peer_id_human_readable(&self) -> Option<String> {
    //     self.remote_peer_id.map(|peer_id| peer_id.human_readable())
    // }

    #[instrument]
    async fn send_handshake(&mut self) -> Result<(), std::io::Error> {
        let peer_id = self.get_peer_id_machine_readable();
        let info_hash = self.get_info_hash();

        self.send_message(peer_protocol::Message::Handshake {
            protocol_extension_bytes: peer_protocol::PROTOCOL_EXTENSION_HEADER,
            peer_id,
            info_hash,
        })
        .await
    }

    #[instrument(skip(self))]
    async fn send_keepalive(&mut self) -> Result<(), std::io::Error> {
        self.send_message(peer_protocol::Message::Keepalive).await
    }

    #[instrument(skip(self))]
    async fn send_choke(&mut self) -> Result<(), std::io::Error> {
        self.send_message(peer_protocol::Message::Choke).await
    }

    #[instrument(skip(self))]
    async fn send_unchoke(&mut self) -> Result<(), std::io::Error> {
        self.send_message(peer_protocol::Message::Unchoke).await
    }

    #[instrument(skip(self))]
    async fn send_interested(&mut self) -> Result<(), std::io::Error> {
        self.send_message(peer_protocol::Message::Interested).await
    }

    #[instrument(skip(self))]
    async fn send_not_interested(&mut self) -> Result<(), std::io::Error> {
        self.send_message(peer_protocol::Message::NotInterested)
            .await
    }

    #[instrument(skip(self))]
    async fn send_have(&mut self, index: Index) -> Result<(), std::io::Error> {
        self.send_message(peer_protocol::Message::Have { index })
            .await
    }

    #[instrument(skip(self))]
    async fn send_bitfield(&mut self, bitfield: BitVec<Lsb0, u8>) -> Result<(), std::io::Error> {
        self.send_message(peer_protocol::Message::Bitfield { bitfield })
            .await
    }

    #[instrument(skip(self))]
    async fn send_request(
        &mut self,
        index: Index,
        begin: Begin,
        length: Length,
    ) -> Result<(), std::io::Error> {
        self.send_message(peer_protocol::Message::Request {
            index,
            begin,
            length,
        })
        .await
    }

    #[instrument(skip(self, chunk))]
    async fn send_piece(
        &mut self,
        index: Index,
        begin: Begin,
        chunk: Vec<u8>,
    ) -> Result<(), std::io::Error> {
        self.send_message(peer_protocol::Message::Piece {
            index,
            begin,
            chunk,
        })
        .await
    }

    #[instrument(skip(self))]
    async fn send_cancel(
        &mut self,
        index: Index,
        begin: Begin,
        length: Length,
    ) -> Result<(), std::io::Error> {
        self.send_message(peer_protocol::Message::Cancel {
            index,
            begin,
            length,
        })
        .await
    }

    async fn send_message(
        &mut self,
        message: peer_protocol::Message,
    ) -> Result<(), std::io::Error> {
        let bytes: Vec<u8> = message.into();
        self.socket.write_all(&bytes).await
    }

    async fn receive_message(&mut self) -> Result<peer_protocol::Message, std::io::Error> {
        let message_length = self.receive_length().await?;

        // TODO: does this need to be fully initialized with 0u8 values
        // in order for it to be filled by `read_exact`?
        let mut buf = Vec::with_capacity(message_length as usize);

        self.socket.read_exact(&mut buf).await?;

        peer_protocol::Message::try_from(buf)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    async fn receive_handshake(&mut self) -> Result<peer_protocol::Message, std::io::Error> {
        // handshake buf has to be fully filled with bytes, not just allocated capacity with `Vec::with_capacity`.
        let mut buf = vec![0u8; HANDSHAKE_LENGTH];
        self.socket.read_exact(&mut buf).await?;

        peer_protocol::Message::try_from(buf)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    async fn receive_length(&mut self) -> Result<u32, std::io::Error> {
        let mut length_bytes: [u8; 4] = [0; 4];
        self.socket.read_exact(&mut length_bytes).await?;
        Ok(u32::from_be_bytes(length_bytes))
    }

    #[instrument(skip(self))]
    async fn register_with_owning_torrent(&mut self) -> Result<(), std::io::Error> {
        let (torrent_to_peer_tx, torrent_to_peer_rx) = tokio::sync::mpsc::channel(32);
        self.torrent_to_peer_rx = Some(torrent_to_peer_rx);
        self.send_to_owned_torrent(messages::PeerToTorrent::Register {
            remote_peer_id: self
                .remote_peer_id
                .expect("remote peer id must be known and set here"),
            torrent_to_peer_tx,
        })
        .await
    }

    #[instrument(skip(self))]
    async fn deregister_with_owning_torrent(&mut self) -> Result<(), std::io::Error> {
        self.send_to_owned_torrent(messages::PeerToTorrent::Deregister {
            remote_peer_id: self.remote_peer_id.unwrap(),
        })
        .await
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

    #[instrument]
    // this exists as a free function because it allows us to do a partial borrow of the Self struct's contents,
    // rather than mutable borrowing the entire Self, which can only be done once in a scope
    async fn receive_message_from_owning_torrent(
        rx: &mut tokio::sync::mpsc::Receiver<messages::TorrentToPeer>,
    ) -> Option<messages::TorrentToPeer> {
        rx.recv().await
    }
}

struct Timers {
    keepalive: Interval,
}

#[derive(Debug)]
enum ChokeState {
    Choked,
    NotChoked,
}

#[derive(Debug)]
enum InterestState {
    Interested,
    NotInterested,
}
