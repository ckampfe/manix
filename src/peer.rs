use std::convert::TryFrom;

use bitvec::{order::Lsb0, prelude::BitVec};
use futures_util::try_join;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

use crate::{
    peer_protocol::{self, HANDSHAKE_LENGTH},
    Begin, Index, InfoHash, Length, PeerId,
};

pub(crate) struct Peer {
    socket: tokio::net::TcpStream,
    peer_id: PeerId,
    remote_peer_id: Option<PeerId>,
    info_hash: InfoHash,
    peer_to_torrent_tx: tokio::sync::mpsc::Sender<PeerToTorrent>,
    handshake_status: HandshakeState,
    global_permit: Option<tokio::sync::OwnedSemaphorePermit>,
    torrent_permit: Option<tokio::sync::OwnedSemaphorePermit>,
    choke_state: ChokeState,
    interest_state: InterestState,
}

impl Peer {
    pub(crate) fn new(
        socket: TcpStream,
        peer_id: PeerId,
        info_hash: InfoHash,
        peer_to_torrent_tx: tokio::sync::mpsc::Sender<PeerToTorrent>,
    ) -> Self {
        Self {
            socket,
            peer_id,
            remote_peer_id: None,
            info_hash,
            peer_to_torrent_tx,
            handshake_status: HandshakeState::PreHandshake,
            global_permit: None,
            torrent_permit: None,
            choke_state: ChokeState::Choked,
            interest_state: InterestState::NotInterested,
        }
    }

    pub(crate) async fn enter_event_loop(
        mut peer: Peer,
        global_permit: tokio::sync::OwnedSemaphorePermit,
        torrent_permit: tokio::sync::OwnedSemaphorePermit,
    ) -> tokio::task::JoinHandle<Result<(), std::io::Error>> {
        peer.global_permit = Some(global_permit);
        peer.torrent_permit = Some(torrent_permit);
        tokio::spawn(async move {
            loop {
                match peer.handshake_status {
                    HandshakeState::PreHandshake => {
                        println!(
                            "prehandshake {}",
                            peer.get_remote_peer_id_human_readable().unwrap()
                        );
                        peer.pre_handshake().await?;
                        println!(
                            "prehandshake {} complete",
                            peer.get_remote_peer_id_human_readable().unwrap()
                        );
                        println!(
                            "this is where the peer would register with the controlling torrent"
                        );
                    }
                    HandshakeState::PostHandshake => peer.post_handhsake().await?,
                }
            }
        })
    }

    async fn pre_handshake(&mut self) -> Result<(), std::io::Error> {
        self.send_handshake().await?;

        let handshake = self.receive_handshake().await?;

        if let peer_protocol::Message::Handshake {
            peer_id: remote_peer_id,
            info_hash: remote_info_hash,
            ..
        } = handshake
        // if the info hashes match, we can proceed
        // if not, sever the connection and drop the semaphore permit
        {
            if self.info_hash == remote_info_hash {
                self.handshake_status = HandshakeState::PostHandshake;
                self.remote_peer_id = Some(remote_peer_id);
                Ok(())
            } else {
                // drop(permit);
                self.global_permit = None; // this will get dropped when self is dropped
                self.torrent_permit = None; // this will get dropped when self is dropped

                Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "remote info_hash did not match local info hash",
                ))
            }
            // otherwise if the message is NOT a handshake, it is invalid,
            // so drop the permit and the connection
        } else {
            // drop(permit);
            self.global_permit = None; // this will get dropped when self is dropped
            self.torrent_permit = None; // this will get dropped when self is dropped

            Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "received a non-handshake message when was expecting handshake",
            ))
        }
    }

    async fn post_handhsake(&mut self) -> Result<(), std::io::Error> {
        todo!("we made it out of the handshake state")
    }

    // pub(crate) async fn connect<A: ToSocketAddrs>(addr: A) -> Result<Peer, std::io::Error> {
    //     let socket = TcpStream::connect(addr).await?;
    //     Ok(Self { socket })
    // }

    fn get_info_hash(&self) -> InfoHash {
        self.info_hash
    }

    fn get_peer_id_machine_readable(&self) -> PeerId {
        self.peer_id
    }

    fn get_peer_id_human_readable(&self) -> String {
        self.peer_id.human_readable()
    }

    fn get_remote_peer_id_human_readable(&self) -> Option<String> {
        self.remote_peer_id.map(|peer_id| peer_id.human_readable())
    }

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

    async fn send_keepalive(&mut self) -> Result<(), std::io::Error> {
        self.send_message(peer_protocol::Message::Keepalive).await
    }

    async fn send_choke(&mut self) -> Result<(), std::io::Error> {
        self.send_message(peer_protocol::Message::Choke).await
    }

    async fn send_unchoke(&mut self) -> Result<(), std::io::Error> {
        self.send_message(peer_protocol::Message::Unchoke).await
    }

    async fn send_interested(&mut self) -> Result<(), std::io::Error> {
        self.send_message(peer_protocol::Message::Interested).await
    }

    async fn send_not_interested(&mut self) -> Result<(), std::io::Error> {
        self.send_message(peer_protocol::Message::NotInterested)
            .await
    }

    async fn send_have(&mut self, index: Index) -> Result<(), std::io::Error> {
        self.send_message(peer_protocol::Message::Have { index })
            .await
    }

    async fn send_bitfield(&mut self, bitfield: BitVec<Lsb0, u8>) -> Result<(), std::io::Error> {
        self.send_message(peer_protocol::Message::Bitfield { bitfield })
            .await
    }

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
        let mut buf = Vec::with_capacity(HANDSHAKE_LENGTH);
        self.socket.read_exact(&mut buf).await?;

        peer_protocol::Message::try_from(buf)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    async fn receive_length(&mut self) -> Result<u32, std::io::Error> {
        let mut length_bytes: [u8; 4] = [0; 4];
        self.socket.read_exact(&mut length_bytes).await?;
        Ok(u32::from_be_bytes(length_bytes))
    }
}

pub(crate) enum PeerToTorrent {}

enum HandshakeState {
    PreHandshake,
    PostHandshake,
}

enum ChokeState {
    Choked,
    NotChoked,
}

enum InterestState {
    Interested,
    NotInterested,
}
