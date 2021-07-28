use std::convert::{TryFrom, TryInto};

use bitvec::prelude::BitVec;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, ToSocketAddrs},
};

use crate::{torrent, Begin, Index, Length};

const HANDSHAKE_LENGTH_LENGTH: usize = 1;
const BITORRENT_PROCOTOL: &str = "BitTorrent protocol";
const PROTOCOL_EXTENSION_HEADER: &[u8] = &[0, 0, 0, 0, 0, 0, 0, 0];
const INFO_HASH_LENGTH: usize = 20;
const PEER_ID_LENGTH: usize = 20;

const HANDSHAKE_LENGTH: usize = HANDSHAKE_LENGTH_LENGTH
    + BITORRENT_PROCOTOL.len()
    + PROTOCOL_EXTENSION_HEADER.len()
    + INFO_HASH_LENGTH
    + PEER_ID_LENGTH;

const KEEPALIVE: &[u8] = &[];
const CHOKE: u8 = 0;
const UNCHOKE: u8 = 1;
const INTERESTED: u8 = 2;
const NOT_INTERESTED: u8 = 3;
const HAVE: u8 = 4;
const BITFIELD: u8 = 5;
const REQUEST: u8 = 6;
const PIECE: u8 = 7;
const CANCEL: u8 = 8;

struct PeerHandshake {
    protocol_extension_header: [u8; 8],
    info_hash: [u8; 20],
    peer_id: [u8; 20],
}

enum PeerMessage {
    /// empty
    Keepalive,
    /// 0
    Choke,
    /// 1
    Unchoke,
    /// 2
    Interested,
    /// 3
    NotInterested,
    /// 4
    Have { index: Index },
    /// 5
    Bitfield {
        bitfield: BitVec<bitvec::order::Lsb0, u8>,
    },
    /// 6
    Request {
        index: Index,
        begin: Begin,
        length: Length,
    },
    /// 7
    Piece {
        index: Index,
        begin: Begin,
        chunk: Vec<u8>,
    },
    /// 8
    Cancel {
        index: Index,
        begin: Begin,
        length: Length,
    },
    Handshake {
        protocol_extension_bytes: [u8; 8],
        peer_id: [u8; 20],
        info_hash: [u8; 20],
    },
}

impl TryFrom<Vec<u8>> for PeerMessage {
    type Error = String;

    #[allow(clippy::many_single_char_names)]
    fn try_from(value: Vec<u8>) -> Result<Self, Self::Error> {
        match &value[..] {
            [] => Ok(PeerMessage::Keepalive),
            [CHOKE] => Ok(PeerMessage::Choke),
            [UNCHOKE] => Ok(PeerMessage::Unchoke),
            [INTERESTED] => Ok(PeerMessage::Interested),
            [NOT_INTERESTED] => Ok(PeerMessage::NotInterested),
            [HAVE, a, b, c, d] => Ok(PeerMessage::Have {
                index: torrent::decode_number([*a, *b, *c, *d]),
            }),
            [BITFIELD, bitfield @ ..] => Ok(PeerMessage::Bitfield {
                bitfield: BitVec::<bitvec::order::Lsb0, u8>::from_slice(bitfield)
                    .map_err(|e| e.to_string())?,
            }),
            [REQUEST, a, b, c, d, e, f, g, h, i, j, k, l] => {
                let index = torrent::decode_number([*a, *b, *c, *d]);
                let begin = torrent::decode_number([*e, *f, *g, *h]);
                let length = torrent::decode_number([*i, *j, *k, *l]);

                Ok(PeerMessage::Request {
                    index,
                    begin,
                    length,
                })
            }
            [PIECE, a, b, c, d, e, f, g, h, chunk @ ..] => {
                let index = torrent::decode_number([*a, *b, *c, *d]);
                let begin = torrent::decode_number([*e, *f, *g, *h]);
                let chunk = chunk.to_owned();

                Ok(PeerMessage::Piece {
                    index,
                    begin,
                    chunk,
                })
            }
            [CANCEL, a, b, c, d, e, f, g, h, i, j, k, l] => {
                let index = torrent::decode_number([*a, *b, *c, *d]);
                let begin = torrent::decode_number([*e, *f, *g, *h]);
                let length = torrent::decode_number([*i, *j, *k, *l]);

                Ok(PeerMessage::Cancel {
                    index,
                    begin,
                    length,
                })
            }
            // '19' + "BitTorrent protocol"
            [19, b'B', b'i', b't', b'T', b'o', b'r', b'r', b'e', b'n', b't', b' ', b'p', b'r', b'o', b't', b'o', b'c', b'o', b'l', rest @ ..] =>
            {
                let (protocol_extension_bytes, rest) = rest.split_at(8);
                let (info_hash, peer_id) = rest.split_at(20);

                let protocol_extension_bytes: [u8; 8] = protocol_extension_bytes
                    .try_into()
                    .expect("Protocol extension bytes must be length 20");
                let peer_id: [u8; 20] = peer_id.try_into().expect("Peer ID must be length 20");
                let info_hash: [u8; 20] =
                    info_hash.try_into().expect("Info hash must be length 20");

                Ok(PeerMessage::Handshake {
                    protocol_extension_bytes,
                    peer_id,
                    info_hash,
                })
            }
            _ => Err("Could not decode bencoded peer message".to_string()),
        }
    }
}

pub(crate) struct Peer {
    socket: tokio::net::TcpStream,
}

impl Peer {
    pub(crate) async fn new(socket: TcpStream) -> Self {
        Self { socket }
    }

    pub(crate) async fn connect<A: ToSocketAddrs>(addr: A) -> Result<Peer, std::io::Error> {
        let socket = TcpStream::connect(addr).await?;
        Ok(Self { socket })
    }
    // def send_handshake(socket, info_hash, peer_id) do
    //   bt = "BitTorrent protocol"

    //   message = [
    //     19,
    //     bt,
    //     0,
    //     0,
    //     0,
    //     0,
    //     0,
    //     0,
    //     0,
    //     0,
    //     info_hash,
    //     peer_id
    //   ]

    //   send_message(
    //     socket,
    //     message
    //   )
    // end
    async fn send_handshake(
        &mut self,
        info_hash: &[u8],
        peer_id: &[u8],
    ) -> Result<(), std::io::Error> {
        let bt = b"BitTorrent protocol";
        let buf = [
            [19u8].as_ref(),
            bt,
            PROTOCOL_EXTENSION_HEADER,
            info_hash,
            peer_id,
        ]
        .concat();
        self.send_message(&buf).await
    }

    //       def send_keepalive(socket) do
    //     send_message(socket, <<>>)
    //   end

    async fn send_keepalive(&mut self) -> Result<(), std::io::Error> {
        self.send_message(KEEPALIVE).await
    }
    //
    //   def send_choke(socket) do
    //     send_message(socket, <<0>>)
    //   end
    async fn send_choke(&mut self) -> Result<(), std::io::Error> {
        self.send_message(&[CHOKE]).await
    }
    //
    //   def send_unchoke(socket) do
    //     send_message(socket, <<1>>)
    //   end

    async fn send_unchoke(&mut self) -> Result<(), std::io::Error> {
        self.send_message(&[UNCHOKE]).await
    }
    //
    //   def send_interested(socket) do
    //     send_message(socket, <<2>>)
    //   end

    async fn send_interested(&mut self) -> Result<(), std::io::Error> {
        self.send_message(&[INTERESTED]).await
    }
    //
    //   def send_not_interested(socket) do
    //     send_message(socket, <<3>>)
    //   end

    async fn send_not_interested(&mut self) -> Result<(), std::io::Error> {
        self.send_message(&[NOT_INTERESTED]).await
    }
    //
    //   def send_have(socket, index) do
    //     encoded_index = Torrent.encode_number(index)
    //     send_message(socket, [4, encoded_index])
    //   end
    async fn send_have(&mut self, index: Index) -> Result<(), std::io::Error> {
        let index_bytes = torrent::encode_number(index);
        let buf = [[HAVE].as_ref(), &index_bytes].concat();
        self.send_message(&buf).await
    }
    //
    //   def send_bitfield(socket, indexes) do
    //     bitfield = Torrent.indexes_to_bitfield(indexes)
    //     send_message(socket, [5, bitfield])
    //   end

    async fn send_bitfield(&mut self, bitfield: &[u8]) -> Result<(), std::io::Error> {
        let buf = [[BITFIELD].as_ref(), bitfield].concat();
        self.send_message(&buf).await
    }
    //
    //   def send_request(socket, index, begin, length) do
    //     send_message(
    //       socket,
    //       [
    //         6,
    //         Torrent.encode_number(index),
    //         Torrent.encode_number(begin),
    //         Torrent.encode_number(length)
    //       ]
    //     )
    //   end
    async fn send_request(
        &mut self,
        index: Index,
        begin: Begin,
        length: Length,
    ) -> Result<(), std::io::Error> {
        let index_bytes = torrent::encode_number(index);
        let begin_bytes = torrent::encode_number(begin);
        let length_bytes = torrent::encode_number(length);

        let buf = [
            [REQUEST].as_ref(),
            &index_bytes,
            &begin_bytes,
            &length_bytes,
        ]
        .concat();

        self.send_message(&buf).await
    }
    //
    //   def send_piece(socket, index, begin, piece) do
    //     send_message(socket, [7, Torrent.encode_number(index), Torrent.encode_number(begin), piece])
    //   end
    async fn send_piece(
        &mut self,
        index: Index,
        begin: Begin,
        piece: &[u8],
    ) -> Result<(), std::io::Error> {
        let index_bytes = torrent::encode_number(index);
        let begin_bytes = torrent::encode_number(begin);

        let buf = [[PIECE].as_ref(), &index_bytes, &begin_bytes, piece].concat();

        self.send_message(&buf).await
    }
    //
    //   def send_cancel(socket, index, begin, length) do
    //     send_message(
    //       socket,
    //       [
    //         8,
    //         Torrent.encode_number(index),
    //         Torrent.encode_number(begin),
    //         Torrent.encode_number(length)
    //       ]
    //     )
    //   end
    async fn send_cancel(
        &mut self,
        index: Index,
        begin: Begin,
        length: Length,
    ) -> Result<(), std::io::Error> {
        let index_bytes = torrent::encode_number(index);
        let begin_bytes = torrent::encode_number(begin);
        let length_bytes = torrent::encode_number(length);

        let buf = [[CANCEL].as_ref(), &index_bytes, &begin_bytes, &length_bytes].concat();

        self.send_message(&buf).await
    }
    //
    //   def send_message(socket, iolist) do
    //     :gen_tcp.send(socket, iolist)
    //   end

    async fn send_message(&mut self, bytes: &[u8]) -> Result<(), std::io::Error> {
        // TODO: investigate using write_vectored to be able to take an arbitrary
        // slice of byte-slices that are then written without needing a reallocation?
        self.socket.write_all(bytes).await
    }

    async fn receive_message(&mut self) -> Result<PeerMessage, std::io::Error> {
        let message_length = self.receive_length().await?;

        // TODO: does this need to be fully initialized with 0u8 values
        // in order for it to be filled by `read_exact`?
        let mut buf = Vec::with_capacity(message_length as usize);

        self.socket.read_exact(&mut buf).await?;

        PeerMessage::try_from(buf)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    async fn receive_length(&mut self) -> Result<u32, std::io::Error> {
        let mut length_bytes: [u8; 4] = [0; 4];
        self.socket.read_exact(&mut length_bytes).await?;
        Ok(u32::from_be_bytes(length_bytes))
    }

    async fn receive_handshake(&mut self) -> Result<PeerHandshake, std::io::Error> {
        let mut buf = Vec::with_capacity(HANDSHAKE_LENGTH);

        self.socket.read_exact(&mut buf).await?;

        // PeerMessage::try_from(buf)
        //     .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
        todo!()
    }
    //
    //   ### RECEIVE
    //
    //   def parse_message(packet) do
    //     case packet do
    //       <<0>> ->
    //         %{type: :choke}
    //
    //       <<1>> ->
    //         %{type: :unchoke}
    //
    //       <<2>> ->
    //         %{type: :interested}
    //
    //       <<3>> ->
    //         %{type: :not_interested}
    //
    //       <<4, index::32-integer-big>> ->
    //         %{type: :have, index: index}
    //
    //       <<5, bitfield::binary()>> ->
    //         %{type: :bitfield, bitfield: bitfield}
    //
    //       <<6, index::32-integer-big, begin::32-integer-big, length::32-integer-big>> ->
    //         %{type: :request, index: index, begin: begin, length: length}
    //
    //       <<7, index::32-integer-big, begin::32-integer-big, chunk::binary()>> ->
    //         %{type: :piece, index: index, begin: begin, chunk: chunk}
    //
    //       <<8, index::32-integer-big, begin::32-integer-big, length::32-integer-big>> ->
    //         %{type: :cancel, index: index, begin: begin, length: length}
    //
    //       <<>> ->
    //         %{type: :keepalive}
    //     end
    //   end
}
