use std::convert::{TryFrom, TryInto};

use crate::{Begin, Index, Length};
use bitvec::prelude::BitVec;

const HANDSHAKE_LENGTH_LENGTH: usize = 1;
const BITORRENT_PROCOTOL: &[u8] = b"BitTorrent protocol";
pub(crate) const PROTOCOL_EXTENSION_HEADER: [u8; 8] = [0, 0, 0, 0, 0, 0, 0, 0];
const INFO_HASH_LENGTH: usize = 20;
const PEER_ID_LENGTH: usize = 20;

pub(crate) const HANDSHAKE_LENGTH: usize = HANDSHAKE_LENGTH_LENGTH
    + BITORRENT_PROCOTOL.len()
    + PROTOCOL_EXTENSION_HEADER.len()
    + INFO_HASH_LENGTH
    + PEER_ID_LENGTH;

pub(crate) const CHOKE: u8 = 0;
pub(crate) const UNCHOKE: u8 = 1;
pub(crate) const INTERESTED: u8 = 2;
pub(crate) const NOT_INTERESTED: u8 = 3;
pub(crate) const HAVE: u8 = 4;
pub(crate) const BITFIELD: u8 = 5;
pub(crate) const REQUEST: u8 = 6;
pub(crate) const PIECE: u8 = 7;
pub(crate) const CANCEL: u8 = 8;

pub(crate) enum PeerMessage {
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

impl From<PeerMessage> for Vec<u8> {
    fn from(msg: PeerMessage) -> Vec<u8> {
        match msg {
            PeerMessage::Keepalive => vec![],
            PeerMessage::Choke => {
                let mut bytes = Vec::with_capacity(5);
                bytes.extend_from_slice(&1u32.to_be_bytes());
                bytes.push(CHOKE);
                bytes
            }
            PeerMessage::Unchoke => {
                let mut bytes = Vec::with_capacity(5);
                bytes.extend_from_slice(&1u32.to_be_bytes());
                bytes.push(UNCHOKE);
                bytes
            }
            PeerMessage::Interested => {
                let mut bytes = Vec::with_capacity(5);
                bytes.extend_from_slice(&1u32.to_be_bytes());
                bytes.push(INTERESTED);
                bytes
            }
            PeerMessage::NotInterested => {
                let mut bytes = Vec::with_capacity(5);
                bytes.extend_from_slice(&1u32.to_be_bytes());
                bytes.push(NOT_INTERESTED);
                bytes
            }
            PeerMessage::Have { index } => {
                let mut bytes = Vec::with_capacity(9);
                bytes.extend_from_slice(&5u32.to_be_bytes());
                bytes.push(HAVE);
                bytes.extend_from_slice(&index.to_be_bytes());
                bytes
            }
            PeerMessage::Bitfield { bitfield } => {
                let bitfield_as_bytes = bitfield.as_raw_slice();
                let mut bytes = Vec::with_capacity(4 + 1 + bitfield_as_bytes.len());
                bytes.extend_from_slice(&encode_number(1 + bitfield_as_bytes.len() as u32));
                bytes.push(BITFIELD);
                bytes.extend_from_slice(bitfield_as_bytes);
                todo!("this bitfield impl is probably wrong and needs to be tested");
                bytes
            }
            PeerMessage::Request {
                index,
                begin,
                length,
            } => {
                let mut bytes = Vec::with_capacity(4 + 1 + 4 + 4 + 4);
                bytes.extend_from_slice(&encode_number(13));
                bytes.push(REQUEST);
                bytes.extend_from_slice(&encode_number(index));
                bytes.extend_from_slice(&encode_number(begin));
                bytes.extend_from_slice(&encode_number(length));
                bytes
            }
            PeerMessage::Piece {
                index,
                begin,
                chunk,
            } => {
                let mut bytes = Vec::with_capacity(4 + 1 + 4 + 4 + chunk.len());
                bytes.extend_from_slice(&encode_number(1 + 4 + 4 + chunk.len() as u32));
                bytes.push(PIECE);
                bytes.extend_from_slice(&encode_number(index));
                bytes.extend_from_slice(&encode_number(begin));
                bytes.extend_from_slice(&chunk);
                bytes
            }
            PeerMessage::Cancel {
                index,
                begin,
                length,
            } => {
                let mut bytes = Vec::with_capacity(4 + 1 + 4 + 4 + 4);
                bytes.extend_from_slice(&encode_number(13));
                bytes.push(CANCEL);
                bytes.extend_from_slice(&encode_number(index));
                bytes.extend_from_slice(&encode_number(begin));
                bytes.extend_from_slice(&encode_number(length));
                bytes
            }
            PeerMessage::Handshake {
                protocol_extension_bytes,
                peer_id,
                info_hash,
            } => {
                let mut bytes = Vec::with_capacity(HANDSHAKE_LENGTH);
                bytes.push(19);
                bytes.extend_from_slice(BITORRENT_PROCOTOL);
                bytes.extend_from_slice(&protocol_extension_bytes);
                bytes.extend_from_slice(&info_hash);
                bytes.extend_from_slice(&peer_id);
                bytes
            }
        }
    }
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
                index: decode_number([*a, *b, *c, *d]),
            }),
            [BITFIELD, bitfield @ ..] => Ok(PeerMessage::Bitfield {
                bitfield: BitVec::<bitvec::order::Lsb0, u8>::from_slice(bitfield)
                    .map_err(|e| e.to_string())?,
            }),
            [REQUEST, a, b, c, d, e, f, g, h, i, j, k, l] => {
                let index = decode_number([*a, *b, *c, *d]);
                let begin = decode_number([*e, *f, *g, *h]);
                let length = decode_number([*i, *j, *k, *l]);

                Ok(PeerMessage::Request {
                    index,
                    begin,
                    length,
                })
            }
            [PIECE, a, b, c, d, e, f, g, h, chunk @ ..] => {
                let index = decode_number([*a, *b, *c, *d]);
                let begin = decode_number([*e, *f, *g, *h]);
                let chunk = chunk.to_owned();

                Ok(PeerMessage::Piece {
                    index,
                    begin,
                    chunk,
                })
            }
            [CANCEL, a, b, c, d, e, f, g, h, i, j, k, l] => {
                let index = decode_number([*a, *b, *c, *d]);
                let begin = decode_number([*e, *f, *g, *h]);
                let length = decode_number([*i, *j, *k, *l]);

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

pub(crate) fn encode_number(n: u32) -> [u8; 4] {
    n.to_be_bytes()
}

pub(crate) fn decode_number(bytes: [u8; 4]) -> u32 {
    u32::from_be_bytes(bytes)
}
