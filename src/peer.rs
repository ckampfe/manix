use std::convert::TryFrom;

use bitvec::{order::Lsb0, prelude::BitVec};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, ToSocketAddrs},
};

use crate::peer_protocol::{
    BITFIELD, CANCEL, CHOKE, HANDSHAKE_LENGTH, HAVE, INTERESTED, NOT_INTERESTED, PIECE,
    PROTOCOL_EXTENSION_HEADER, REQUEST, UNCHOKE,
};
use crate::{peer_protocol, torrent, Begin, Index, Length};

pub(crate) struct Peer {
    socket: tokio::net::TcpStream,
    peer_id: [u8; 20],
    info_hash: [u8; 20],
}

impl Peer {
    pub(crate) async fn new(socket: TcpStream, peer_id: [u8; 20], info_hash: [u8; 20]) -> Self {
        Self {
            socket,
            peer_id,
            info_hash,
        }
    }

    // pub(crate) async fn connect<A: ToSocketAddrs>(addr: A) -> Result<Peer, std::io::Error> {
    //     let socket = TcpStream::connect(addr).await?;
    //     Ok(Self { socket })
    // }

    fn get_info_hash(&self) -> [u8; 20] {
        self.info_hash
    }

    fn get_peer_id_machine_readable(&self) -> [u8; 20] {
        self.peer_id
    }

    async fn send_handshake(&mut self) -> Result<(), std::io::Error> {
        let peer_id = self.get_peer_id_machine_readable();
        let info_hash = self.get_info_hash();

        self.send_message(peer_protocol::PeerMessage::Handshake {
            protocol_extension_bytes: PROTOCOL_EXTENSION_HEADER,
            peer_id,
            info_hash,
        })
        .await
    }

    async fn send_keepalive(&mut self) -> Result<(), std::io::Error> {
        self.send_message(peer_protocol::PeerMessage::Keepalive)
            .await
    }

    async fn send_choke(&mut self) -> Result<(), std::io::Error> {
        self.send_message(peer_protocol::PeerMessage::Choke).await
    }

    async fn send_unchoke(&mut self) -> Result<(), std::io::Error> {
        self.send_message(peer_protocol::PeerMessage::Unchoke).await
    }

    async fn send_interested(&mut self) -> Result<(), std::io::Error> {
        self.send_message(peer_protocol::PeerMessage::Interested)
            .await
    }

    async fn send_not_interested(&mut self) -> Result<(), std::io::Error> {
        self.send_message(peer_protocol::PeerMessage::NotInterested)
            .await
    }

    async fn send_have(&mut self, index: Index) -> Result<(), std::io::Error> {
        self.send_message(peer_protocol::PeerMessage::Have { index })
            .await
    }

    async fn send_bitfield(&mut self, bitfield: BitVec<Lsb0, u8>) -> Result<(), std::io::Error> {
        self.send_message(peer_protocol::PeerMessage::Bitfield { bitfield })
            .await
    }

    async fn send_request(
        &mut self,
        index: Index,
        begin: Begin,
        length: Length,
    ) -> Result<(), std::io::Error> {
        self.send_message(peer_protocol::PeerMessage::Request {
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
        self.send_message(peer_protocol::PeerMessage::Piece {
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
        let index_bytes = peer_protocol::encode_number(index);
        let begin_bytes = peer_protocol::encode_number(begin);
        let length_bytes = peer_protocol::encode_number(length);

        let buf = [[CANCEL].as_ref(), &index_bytes, &begin_bytes, &length_bytes].concat();

        self.send_message(peer_protocol::PeerMessage::Cancel {
            index,
            begin,
            length,
        })
        .await
    }

    async fn send_message(
        &mut self,
        message: peer_protocol::PeerMessage,
    ) -> Result<(), std::io::Error> {
        let bytes: Vec<u8> = message.into();
        self.socket.write_all(&bytes).await
    }

    async fn receive_message(&mut self) -> Result<peer_protocol::PeerMessage, std::io::Error> {
        let message_length = self.receive_length().await?;

        // TODO: does this need to be fully initialized with 0u8 values
        // in order for it to be filled by `read_exact`?
        let mut buf = Vec::with_capacity(message_length as usize);

        self.socket.read_exact(&mut buf).await?;

        peer_protocol::PeerMessage::try_from(buf)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    async fn receive_length(&mut self) -> Result<u32, std::io::Error> {
        let mut length_bytes: [u8; 4] = [0; 4];
        self.socket.read_exact(&mut length_bytes).await?;
        Ok(u32::from_be_bytes(length_bytes))
    }
}
