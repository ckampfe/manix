use crate::{Begin, Index, InfoHash, Length, PeerId};
use bitvec::order::Msb0;
use bitvec::prelude::{bitvec, BitVec};
use bytes::Buf;
use std::convert::{TryFrom, TryInto};
use std::ops::BitOrAssign;
use std::ops::{Deref, DerefMut};

const HANDSHAKE_LENGTH_LENGTH: usize = 1;
const BITTORRENT_PROTOCOL: &[u8] = b"BitTorrent protocol";
pub(crate) const PROTOCOL_EXTENSION_HEADER: [u8; 8] = [0, 0, 0, 0, 0, 0, 0, 0];
const INFO_HASH_LENGTH: usize = 20;
const PEER_ID_LENGTH: usize = 20;

pub(crate) const HANDSHAKE_LENGTH: usize = HANDSHAKE_LENGTH_LENGTH
    + BITTORRENT_PROTOCOL.len()
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

#[derive(Debug)]
pub(crate) struct MessageCodec;

const MAX: usize = 8 * 1024 * 1024;

impl tokio_util::codec::Decoder for MessageCodec {
    type Item = Message;

    type Error = std::io::Error;

    fn decode(&mut self, src: &mut bytes::BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.len() < 4 {
            // Not enough data to read length marker.
            return Ok(None);
        }

        // Read length marker.
        let mut length_bytes = [0u8; 4];
        length_bytes.copy_from_slice(&src[..4]);
        let length = u32::from_le_bytes(length_bytes) as usize;

        // Check that the length is not too large to avoid a denial of
        // service attack where the server runs out of memory.
        if length > MAX {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Frame of length {} is too large.", length),
            ));
        }

        if src.len() < 4 + length {
            // The full string has not yet arrived.
            //
            // We reserve more space in the buffer. This is not strictly
            // necessary, but is a good idea performance-wise.
            src.reserve(4 + length - src.len());

            // We inform the Framed that we need more bytes to form the next
            // frame.
            return Ok(None);
        }
        // Use advance to modify src such that it no longer contains
        // this frame.
        let data = src[4..4 + length].to_vec();
        src.advance(4 + length);

        let message = Message::try_from(data).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "couldn't form message")
        })?;

        Ok(Some(message))
    }
}

impl tokio_util::codec::Encoder<Message> for MessageCodec {
    type Error = std::io::Error;

    fn encode(&mut self, item: Message, dst: &mut bytes::BytesMut) -> Result<(), Self::Error> {
        // if item.len() > MAX {
        //     return Err(std::io::Error::new(
        //         std::io::ErrorKind::InvalidData,
        //         format!("Frame of length {} is too large.", item.len()),
        //     ));
        // }
        // Convert the length into a byte array.
        // The cast to u32 cannot overflow due to the length check above.

        // let len_slice = u32::to_le_bytes(item.len() as u32);
        // Reserve space in the buffer.
        let as_bytes: Vec<u8> = item.into();
        // dst.reserve(4 + item.len());
        dst.reserve(as_bytes.len());

        // Write the length and string to the buffer.
        // dst.extend_from_slice(&len_slice);
        dst.extend_from_slice(&as_bytes);
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Message {
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
    Bitfield { bitfield: Bitfield },
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
        peer_id: PeerId,
        info_hash: InfoHash,
    },
}

impl From<Message> for Vec<u8> {
    fn from(msg: Message) -> Vec<u8> {
        match msg {
            Message::Keepalive => vec![],
            Message::Choke => {
                let mut bytes = Vec::with_capacity(5);
                bytes.extend_from_slice(&1u32.to_be_bytes());
                bytes.push(CHOKE);
                bytes
            }
            Message::Unchoke => {
                let mut bytes = Vec::with_capacity(5);
                bytes.extend_from_slice(&1u32.to_be_bytes());
                bytes.push(UNCHOKE);
                bytes
            }
            Message::Interested => {
                let mut bytes = Vec::with_capacity(5);
                bytes.extend_from_slice(&1u32.to_be_bytes());
                bytes.push(INTERESTED);
                bytes
            }
            Message::NotInterested => {
                let mut bytes = Vec::with_capacity(5);
                bytes.extend_from_slice(&1u32.to_be_bytes());
                bytes.push(NOT_INTERESTED);
                bytes
            }
            Message::Have { index } => {
                let mut bytes = Vec::with_capacity(9);
                bytes.extend_from_slice(&5u32.to_be_bytes());
                bytes.push(HAVE);
                bytes.extend_from_slice(&index.to_be_bytes());
                bytes
            }
            Message::Bitfield { bitfield } => {
                let bitfield_as_bytes = bitfield.as_raw_slice();
                let mut bytes = Vec::with_capacity(4 + 1 + bitfield_as_bytes.len());
                bytes.extend_from_slice(&encode_number(1 + bitfield_as_bytes.len() as u32));
                bytes.push(BITFIELD);
                bytes.extend_from_slice(bitfield_as_bytes);
                bytes
            }
            Message::Request {
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
            Message::Piece {
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
            Message::Cancel {
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
            Message::Handshake {
                protocol_extension_bytes,
                peer_id,
                info_hash,
            } => {
                let mut bytes = Vec::with_capacity(HANDSHAKE_LENGTH);
                bytes.push(19);
                bytes.extend_from_slice(BITTORRENT_PROTOCOL);
                bytes.extend_from_slice(&protocol_extension_bytes);
                bytes.extend_from_slice(info_hash.as_ref());
                bytes.extend_from_slice(peer_id.as_ref());
                bytes
            }
        }
    }
}

impl TryFrom<Vec<u8>> for Message {
    type Error = String;

    #[allow(clippy::many_single_char_names)]
    fn try_from(value: Vec<u8>) -> Result<Self, Self::Error> {
        match &value[..] {
            [] => Ok(Message::Keepalive),
            [CHOKE] => Ok(Message::Choke),
            [UNCHOKE] => Ok(Message::Unchoke),
            [INTERESTED] => Ok(Message::Interested),
            [NOT_INTERESTED] => Ok(Message::NotInterested),
            [HAVE, a, b, c, d] => Ok(Message::Have {
                index: decode_number([*a, *b, *c, *d]),
            }),
            [BITFIELD, bitfield @ ..] => Ok(Message::Bitfield {
                bitfield: Bitfield(
                    BitVec::<bitvec::order::Msb0, u8>::from_slice(bitfield)
                        .map_err(|e| e.to_string())?,
                ),
            }),
            [REQUEST, a, b, c, d, e, f, g, h, i, j, k, l] => {
                let index = decode_number([*a, *b, *c, *d]);
                let begin = decode_number([*e, *f, *g, *h]);
                let length = decode_number([*i, *j, *k, *l]);

                Ok(Message::Request {
                    index,
                    begin,
                    length,
                })
            }
            [PIECE, a, b, c, d, e, f, g, h, chunk @ ..] => {
                let index = decode_number([*a, *b, *c, *d]);
                let begin = decode_number([*e, *f, *g, *h]);
                let chunk = chunk.to_owned();

                Ok(Message::Piece {
                    index,
                    begin,
                    chunk,
                })
            }
            [CANCEL, a, b, c, d, e, f, g, h, i, j, k, l] => {
                let index = decode_number([*a, *b, *c, *d]);
                let begin = decode_number([*e, *f, *g, *h]);
                let length = decode_number([*i, *j, *k, *l]);

                Ok(Message::Cancel {
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
                let peer_id: PeerId =
                    PeerId(peer_id[..20].try_into().expect("Peer ID must be length 20"));
                let info_hash: InfoHash =
                    InfoHash(info_hash.try_into().expect("Info hash must be length 20"));

                Ok(Message::Handshake {
                    protocol_extension_bytes,
                    peer_id,
                    info_hash,
                })
            }
            _ => Err("Could not decode bencoded peer message".to_string()),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Bitfield(BitVec<Msb0, u8>);

impl Bitfield {
    pub(crate) fn new(length: usize) -> Self {
        Self(bitvec![Msb0, u8; 0; length])
    }
}

impl BitOrAssign for Bitfield {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0.bitor_assign(rhs.0);
    }
}

impl Deref for Bitfield {
    type Target = BitVec<Msb0, u8>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Bitfield {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

pub(crate) fn encode_number(n: u32) -> [u8; 4] {
    n.to_be_bytes()
}

pub(crate) fn decode_number(bytes: [u8; 4]) -> u32 {
    u32::from_be_bytes(bytes)
}

// Torrents are composed of "pieces".
// Pieces have a 20-byte SHA-1 hash.
// Pieces are composed of an undocumented subunit called "chunks".
// When a peer wants a piece, the peer has to ask for:
// - The piece index
// - The chunk offset within the piece (starting at byte 0)
// - The length of the chunk
pub(crate) fn chunk_offsets_lengths(
    piece_length: usize,
    chunk_length: usize,
) -> Vec<(usize, usize)> {
    let mut remaining_length = piece_length;
    let mut i = 0;
    let mut offsets = vec![];

    let chunks_per_piece = piece_length / chunk_length;
    dbg!(chunks_per_piece);

    while i < chunks_per_piece {
        let offset = chunk_length * i;
        offsets.push((offset, chunk_length));
        i += 1;
        remaining_length -= chunk_length;
    }

    if remaining_length > 0 {
        let offset = chunk_length * i;
        offsets.push((offset, remaining_length));
    }

    offsets
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitvec::prelude::*;

    #[test]
    fn encode_numbers() {
        assert_eq!(encode_number(10), [0, 0, 0, 10]);
        assert_eq!(encode_number(u32::MIN), [0, 0, 0, 0]);
        assert_eq!(encode_number(u32::MAX), [255, 255, 255, 255]);
    }

    #[test]
    fn encode_keepalive() {
        let m = Message::Keepalive;
        let encoded: Vec<u8> = m.into();
        assert_eq!(encoded, Vec::<u8>::new());
    }

    #[test]
    fn encode_choke() {
        let m = Message::Choke;
        let encoded: Vec<u8> = m.into();
        assert_eq!(encoded, vec![0, 0, 0, 1, CHOKE]);
    }

    #[test]
    fn encode_unchoke() {
        let m = Message::Unchoke;
        let encoded: Vec<u8> = m.into();
        assert_eq!(encoded, vec![0, 0, 0, 1, UNCHOKE]);
    }

    #[test]
    fn encode_interested() {
        let m = Message::Interested;
        let encoded: Vec<u8> = m.into();
        assert_eq!(encoded, vec![0, 0, 0, 1, INTERESTED]);
    }

    #[test]
    fn encode_not_interested() {
        let m = Message::NotInterested;
        let encoded: Vec<u8> = m.into();
        assert_eq!(encoded, vec![0, 0, 0, 1, NOT_INTERESTED]);
    }

    #[test]
    fn encode_have() {
        let m = Message::Have { index: 63 };
        let encoded: Vec<u8> = m.into();

        let mut expected = vec![];

        let expected_index = encode_number(63);

        expected.extend_from_slice(&encode_number(1 + expected_index.len() as u32));
        expected.push(HAVE);
        expected.extend_from_slice(&expected_index);

        assert_eq!(encoded, expected);
    }

    #[test]
    fn encode_bitfield() {
        let bitfield = Bitfield(bitvec![Msb0, u8; 0, 0, 0, 1, 0, 1, 0, 0]);
        let bitfield_as_bytes = bitfield.as_raw_slice().to_vec();

        let m = Message::Bitfield { bitfield };

        let encoded: Vec<u8> = m.into();

        let mut expected = vec![];
        expected.extend_from_slice(&encode_number(1 + bitfield_as_bytes.len() as u32));
        expected.push(BITFIELD);
        expected.extend_from_slice(&bitfield_as_bytes);

        assert_eq!(encoded, expected);
    }

    #[test]
    fn encode_request() {
        let index = 11;
        let begin = 0;
        let length = 2u32.pow(14);

        let m = Message::Request {
            index,
            begin,
            length,
        };

        let encoded: Vec<u8> = m.into();

        let expected_index = encode_number(index);
        let expected_begin = encode_number(begin);
        let expected_length = encode_number(length);

        let mut expected = vec![];

        expected.extend_from_slice(&encode_number(
            1 + expected_index.len() as u32
                + expected_begin.len() as u32
                + expected_length.len() as u32,
        ));
        expected.push(REQUEST);
        expected.extend_from_slice(&expected_index);
        expected.extend_from_slice(&expected_begin);
        expected.extend_from_slice(&expected_length);

        assert_eq!(encoded, expected);
    }

    #[test]
    fn encode_piece() {
        let index = 31;
        let begin = 2u32.pow(14);
        let chunk = vec![1; 2usize.pow(14)];
        let cloned_chunk = chunk.clone();

        let m = Message::Piece {
            index,
            begin,
            chunk,
        };

        let encoded: Vec<u8> = m.into();

        let expected_index = encode_number(index);
        let expected_begin = encode_number(begin);

        let mut expected = vec![];

        expected.extend_from_slice(&encode_number(
            1 + expected_index.len() as u32
                + expected_begin.len() as u32
                + cloned_chunk.len() as u32,
        ));
        expected.push(PIECE);
        expected.extend_from_slice(&expected_index);
        expected.extend_from_slice(&expected_begin);
        expected.extend_from_slice(&cloned_chunk);

        assert_eq!(encoded, expected);
    }

    #[test]
    fn encode_cancel() {
        let index = 11;
        let begin = 0;
        let length = 2u32.pow(14);

        let m = Message::Cancel {
            index,
            begin,
            length,
        };

        let encoded: Vec<u8> = m.into();

        let expected_index = encode_number(index);
        let expected_begin = encode_number(begin);
        let expected_length = encode_number(length);

        let mut expected = vec![];

        expected.extend_from_slice(&encode_number(
            1 + expected_index.len() as u32
                + expected_begin.len() as u32
                + expected_length.len() as u32,
        ));
        expected.push(CANCEL);
        expected.extend_from_slice(&expected_index);
        expected.extend_from_slice(&expected_begin);
        expected.extend_from_slice(&expected_length);

        assert_eq!(encoded, expected);
    }

    #[test]
    fn encode_handshake() {
        let peer_id = PeerId([
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19,
        ]);
        let info_hash = InfoHash([
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19,
        ]);

        let m = Message::Handshake {
            protocol_extension_bytes: PROTOCOL_EXTENSION_HEADER,
            peer_id,
            info_hash,
        };

        let encoded: Vec<u8> = m.into();

        let mut expected = vec![];

        expected.push(19);
        expected.extend_from_slice(&BITTORRENT_PROTOCOL);
        expected.extend_from_slice(&PROTOCOL_EXTENSION_HEADER);
        expected.extend_from_slice(peer_id.as_ref());
        expected.extend_from_slice(&info_hash.as_ref());

        assert_eq!(encoded, expected);
    }

    #[test]
    fn decode_keepalive() {
        let m = vec![];
        assert_eq!(Message::try_from(m).unwrap(), Message::Keepalive);
    }

    #[test]
    fn decode_choke() {
        let m = vec![CHOKE];
        assert_eq!(Message::try_from(m).unwrap(), Message::Choke);
    }

    #[test]
    fn decode_unchoke() {
        let m = vec![UNCHOKE];
        assert_eq!(Message::try_from(m).unwrap(), Message::Unchoke);
    }

    #[test]
    fn decode_interested() {
        let m = vec![INTERESTED];
        assert_eq!(Message::try_from(m).unwrap(), Message::Interested);
    }

    #[test]
    fn decode_not_interested() {
        let m = vec![NOT_INTERESTED];
        assert_eq!(Message::try_from(m).unwrap(), Message::NotInterested);
    }

    #[test]
    fn decode_have() {
        let m = vec![HAVE, 0, 0, 0, 44];
        assert_eq!(Message::try_from(m).unwrap(), Message::Have { index: 44 });
    }

    #[test]
    fn decode_bitfield() {
        let bitfield = Bitfield(bitvec![Msb0, u8; 0, 0, 1, 0, 1, 0, 0, 0]);
        let m = vec![BITFIELD, 40];
        assert_eq!(
            Message::try_from(m).unwrap(),
            Message::Bitfield { bitfield }
        );
    }

    #[test]
    fn decode_request() {
        let mut m = vec![];

        let index = 11;
        let begin = 0;
        let length = 2u32.pow(14);

        m.push(REQUEST);
        m.extend_from_slice(&encode_number(index));
        m.extend_from_slice(&encode_number(begin));
        m.extend_from_slice(&encode_number(length));

        assert_eq!(
            Message::try_from(m).unwrap(),
            Message::Request {
                index,
                begin,
                length
            }
        )
    }

    #[test]
    fn decode_piece() {
        let mut m = vec![];

        let index = 31;
        let begin = 2u32.pow(14);
        let chunk = vec![1; 2usize.pow(14)];
        let cloned_chunk = chunk.clone();

        let expected_index = encode_number(index);
        let expected_begin = encode_number(begin);

        m.push(PIECE);
        m.extend_from_slice(&expected_index);
        m.extend_from_slice(&expected_begin);
        m.extend_from_slice(&cloned_chunk);

        assert_eq!(
            Message::try_from(m).unwrap(),
            Message::Piece {
                index,
                begin,
                chunk
            }
        )
    }

    #[test]
    fn decode_cancel() {
        let mut m = vec![];

        let index = 11;
        let begin = 0;
        let length = 2u32.pow(14);

        m.push(CANCEL);
        m.extend_from_slice(&encode_number(index));
        m.extend_from_slice(&encode_number(begin));
        m.extend_from_slice(&encode_number(length));

        assert_eq!(
            Message::try_from(m).unwrap(),
            Message::Cancel {
                index,
                begin,
                length
            }
        )
    }

    #[test]
    fn decode_handshake() {
        let mut m = vec![];

        let peer_id = PeerId([
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19,
        ]);
        let info_hash = InfoHash([
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19,
        ]);

        m.push(19);
        m.extend_from_slice(BITTORRENT_PROTOCOL);
        m.extend_from_slice(&PROTOCOL_EXTENSION_HEADER);
        m.extend_from_slice(peer_id.as_ref());
        m.extend_from_slice(info_hash.as_ref());
        assert_eq!(
            Message::try_from(m).unwrap(),
            Message::Handshake {
                protocol_extension_bytes: PROTOCOL_EXTENSION_HEADER,
                peer_id,
                info_hash
            }
        );
    }

    const THIRTY_TWO_K: usize = 2usize.pow(15);
    const SIXTY_FOUR_K: usize = 2usize.pow(16);
    const ONE_HUNDRED_TWENTY_EIGHT_K: usize = 2usize.pow(17);

    #[test]
    fn offsets() {
        // piece of length SIXTY_FOUR_K
        let offsets = chunk_offsets_lengths(SIXTY_FOUR_K, THIRTY_TWO_K);
        assert_eq!(
            offsets,
            vec![(0, THIRTY_TWO_K), (THIRTY_TWO_K, THIRTY_TWO_K)]
        );

        // regular piece of length ONE_HUNDRED_TWENTY_EIGHT_K
        let offsets = chunk_offsets_lengths(ONE_HUNDRED_TWENTY_EIGHT_K, THIRTY_TWO_K);
        assert_eq!(
            offsets,
            vec![
                (0, THIRTY_TWO_K),
                (THIRTY_TWO_K, THIRTY_TWO_K),
                (THIRTY_TWO_K * 2, THIRTY_TWO_K),
                (THIRTY_TWO_K * 3, THIRTY_TWO_K)
            ]
        );

        // irregular piece of length SIXTY_FOUR_K + 10
        // note the lengths of the chunks: the length of the last chunk is 10
        let offsets = chunk_offsets_lengths(SIXTY_FOUR_K + 10, THIRTY_TWO_K);
        assert_eq!(
            offsets,
            vec![
                (0, THIRTY_TWO_K),
                (THIRTY_TWO_K, THIRTY_TWO_K),
                (THIRTY_TWO_K * 2, 10)
            ]
        );
    }
}
