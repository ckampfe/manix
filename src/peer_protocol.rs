use crate::{Begin, Index, InfoHash, Length, PeerId};
use bitvec::order::Msb0;
use bitvec::prelude::{bitvec, BitVec};
use bytes::{Buf, BufMut, BytesMut};
use std::convert::{TryFrom, TryInto};
use std::ops::BitOrAssign;
use std::ops::{Deref, DerefMut};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::{Decoder, Encoder, Framed};

const MAX_MESSAGE_LENGTH: usize = 8 * 1024 * 1024;
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

#[derive(Debug, PartialEq)]
pub(crate) struct Handshake {
    pub(crate) protocol_extension_bytes: [u8; 8],
    pub(crate) peer_id: PeerId,
    pub(crate) info_hash: InfoHash,
}

impl Handshake {
    fn write_to_bytes(&self, buf: &mut BytesMut) {
        buf.reserve(HANDSHAKE_LENGTH);
        buf.put_u8(19);
        buf.extend_from_slice(BITTORRENT_PROTOCOL);
        buf.extend_from_slice(&self.protocol_extension_bytes);
        buf.extend_from_slice(self.info_hash.as_ref());
        buf.extend_from_slice(self.peer_id.as_ref());
    }
}

impl TryFrom<&[u8]> for Handshake {
    type Error = String;

    #[allow(clippy::many_single_char_names)]
    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        match value {
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

                Ok(Handshake {
                    protocol_extension_bytes,
                    peer_id,
                    info_hash,
                })
            }
            _ => Err("Could not decode bencoded peer message".to_string()),
        }
    }
}

#[derive(Debug)]
pub(crate) struct HandshakeCodec;

impl tokio_util::codec::Decoder for HandshakeCodec {
    type Item = Handshake;

    type Error = std::io::Error;

    fn decode(&mut self, src: &mut bytes::BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.len() < HANDSHAKE_LENGTH {
            src.reserve(HANDSHAKE_LENGTH);
            return Ok(None);
        }

        let data = src[0..HANDSHAKE_LENGTH].to_vec();

        src.advance(HANDSHAKE_LENGTH);

        let handshake = Handshake::try_from(&data[..])
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        Ok(Some(handshake))
    }
}

impl tokio_util::codec::Encoder<Handshake> for HandshakeCodec {
    type Error = std::io::Error;

    fn encode(&mut self, item: Handshake, dst: &mut bytes::BytesMut) -> Result<(), Self::Error> {
        item.write_to_bytes(dst);
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Message {
    /// no tag byte
    Keepalive,
    /// tag byte = 0
    Choke,
    /// tag byte = 1
    Unchoke,
    /// tag byte = 2
    Interested,
    /// tag byte = 3
    NotInterested,
    /// tag byte = 4
    Have { index: Index },
    /// tag byte = 5
    Bitfield { bitfield: Bitfield },
    /// tag byte = 6
    Request {
        index: Index,
        begin: Begin,
        length: Length,
    },
    /// tag byte = 7
    Piece {
        index: Index,
        begin: Begin,
        chunk: Vec<u8>,
    },
    /// tag byte = 8
    Cancel {
        index: Index,
        begin: Begin,
        length: Length,
    },
}

impl Message {
    /// in the bittorrent protocol,
    /// messages are of the form "LTV", or Length Type Value,
    /// where Length is a 4 byte big endian u32,
    /// Type is a single byte, and Value is a sequence of bytes
    /// of length Length - 1 (for the type tag byte).
    /// Keepalive alone has no Tag byte or Value, only a four-byte length of 0,
    /// i.e., "0u8 0u8 0u8 0u8"
    fn len(&self) -> usize {
        // type length + value length
        // only keepalive has no type length or value length
        match self {
            Message::Keepalive => 0,
            Message::Choke => 1,
            Message::Unchoke => 1,
            Message::Interested => 1,
            Message::NotInterested => 1,
            Message::Have { .. } => 1 + 4,
            Message::Bitfield { bitfield } => 1 + bitfield.as_raw_slice().len(),
            Message::Request { .. } => 1 + 4 + 4 + 4,
            Message::Piece { chunk, .. } => 1 + 4 + 4 + chunk.len(),
            Message::Cancel { .. } => 1 + 4 + 4 + 4,
        }
    }

    /// Length Type Value aka "LTV"
    /// 4 length bytes, 1 type byte, value bytes
    fn write_to_bytes(&self, buf: &mut BytesMut) {
        let len_len = 4;
        let value_len = self.len();
        let len_slice = u32::to_be_bytes(value_len as u32);
        buf.reserve(len_len + value_len);
        buf.extend_from_slice(&len_slice);

        match self {
            Message::Keepalive => (),
            Message::Choke => {
                buf.put_u8(CHOKE);
            }
            Message::Unchoke => {
                buf.put_u8(UNCHOKE);
            }
            Message::Interested => {
                buf.put_u8(INTERESTED);
            }
            Message::NotInterested => {
                buf.put_u8(NOT_INTERESTED);
            }
            Message::Have { index } => {
                buf.put_u8(HAVE);
                buf.extend_from_slice(&index.to_be_bytes());
            }
            Message::Bitfield { bitfield } => {
                let bitfield_as_bytes = bitfield.as_raw_slice();
                buf.put_u8(BITFIELD);
                buf.extend_from_slice(bitfield_as_bytes);
            }
            Message::Request {
                index,
                begin,
                length,
            } => {
                buf.put_u8(REQUEST);
                buf.extend_from_slice(&encode_number(*index));
                buf.extend_from_slice(&encode_number(*begin));
                buf.extend_from_slice(&encode_number(*length));
            }
            Message::Piece {
                index,
                begin,
                chunk,
            } => {
                buf.put_u8(PIECE);
                buf.extend_from_slice(&encode_number(*index));
                buf.extend_from_slice(&encode_number(*begin));
                buf.extend_from_slice(chunk);
            }
            Message::Cancel {
                index,
                begin,
                length,
            } => {
                buf.put_u8(CANCEL);
                buf.extend_from_slice(&encode_number(*index));
                buf.extend_from_slice(&encode_number(*begin));
                buf.extend_from_slice(&encode_number(*length));
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
            _ => Err("Could not decode bencoded peer message".to_string()),
        }
    }
}

#[derive(Debug)]
pub(crate) struct MessageCodec;

// ideally, this should be split into two codecs:
// one for the handshake and one for the regular frames
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
        let length = u32::from_be_bytes(length_bytes) as usize;

        // Check that the length is not too large to avoid a denial of
        // service attack where the server runs out of memory.
        if length > MAX_MESSAGE_LENGTH {
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

        let message = Message::try_from(data)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        Ok(Some(message))
    }
}

impl tokio_util::codec::Encoder<Message> for MessageCodec {
    type Error = std::io::Error;

    fn encode(&mut self, item: Message, dst: &mut bytes::BytesMut) -> Result<(), Self::Error> {
        item.write_to_bytes(dst);
        Ok(())
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

pub(crate) fn set_codec<T: AsyncRead + AsyncWrite, C1, E, C2: Encoder<E> + Decoder>(
    framed: Framed<T, C1>,
    codec: C2,
) -> Framed<T, C2> {
    let parts1 = framed.into_parts();
    let mut parts2 = Framed::new(parts1.io, codec).into_parts();
    parts2.read_buf = parts1.read_buf;
    parts2.write_buf = parts1.write_buf;
    Framed::from_parts(parts2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitvec::prelude::*;

    mod helpers {
        use super::*;

        pub(crate) fn encode(message: Message) -> Vec<u8> {
            let mut codec = MessageCodec;
            let mut buf = BytesMut::new();
            codec.encode(message, &mut buf).unwrap();
            buf.to_vec()
        }

        pub(crate) fn decode(bytes: &[u8]) -> Message {
            let mut bytes = BytesMut::from(bytes);
            let mut codec = MessageCodec;
            codec.decode(&mut bytes).unwrap().unwrap()
        }
    }

    mod encoding {
        use super::helpers::encode;
        use super::*;

        #[test]
        fn encode_numbers() {
            assert_eq!(encode_number(10), [0, 0, 0, 10]);
            assert_eq!(encode_number(u32::MIN), [0, 0, 0, 0]);
            assert_eq!(encode_number(u32::MAX), [255, 255, 255, 255]);
        }

        #[test]
        fn encode_keepalive() {
            let encoded = encode(Message::Keepalive);
            assert_eq!(encoded, vec![0, 0, 0, 0]);
        }

        #[test]
        fn encode_choke() {
            let encoded = encode(Message::Choke);
            assert_eq!(encoded, vec![0, 0, 0, 1, CHOKE]);
        }

        #[test]
        fn encode_unchoke() {
            let encoded = encode(Message::Unchoke);
            assert_eq!(encoded, vec![0, 0, 0, 1, UNCHOKE]);
        }

        #[test]
        fn encode_interested() {
            let encoded = encode(Message::Interested);
            assert_eq!(encoded, vec![0, 0, 0, 1, INTERESTED]);
        }

        #[test]
        fn encode_not_interested() {
            let encoded = encode(Message::NotInterested);
            assert_eq!(encoded, vec![0, 0, 0, 1, NOT_INTERESTED]);
        }

        #[test]
        fn encode_have() {
            let message = Message::Have { index: 63 };
            let encoded = encode(message);

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

            let message = Message::Bitfield { bitfield };

            let encoded = encode(message);

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

            let message = Message::Request {
                index,
                begin,
                length,
            };

            let encoded = encode(message);

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

            let message = Message::Piece {
                index,
                begin,
                chunk,
            };

            let encoded = encode(message);

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

            let message = Message::Cancel {
                index,
                begin,
                length,
            };

            let encoded = encode(message);

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
            let peer_id = PeerId(rand::random());
            let info_hash = InfoHash(rand::random());

            let mut expected = vec![];

            expected.push(19);
            expected.extend_from_slice(&BITTORRENT_PROTOCOL);
            expected.extend_from_slice(&PROTOCOL_EXTENSION_HEADER);
            expected.extend_from_slice(info_hash.as_ref());
            expected.extend_from_slice(peer_id.as_ref());

            let message = Handshake {
                protocol_extension_bytes: PROTOCOL_EXTENSION_HEADER,
                peer_id,
                info_hash,
            };

            let mut codec = HandshakeCodec;
            let mut buf = BytesMut::new();
            codec.encode(message, &mut buf).unwrap();
            let encoded = buf.to_vec();

            assert_eq!(encoded, expected);
        }
    }

    mod decoding {
        use super::helpers::decode;
        use super::*;

        #[test]
        fn decode_keepalive() {
            let buf = vec![0, 0, 0, 0];
            assert_eq!(decode(&buf), Message::Keepalive);
        }

        #[test]
        fn decode_choke() {
            let buf = vec![0, 0, 0, 1, CHOKE];
            assert_eq!(decode(&buf), Message::Choke);
        }

        #[test]
        fn decode_unchoke() {
            let buf = vec![0, 0, 0, 1, UNCHOKE];
            assert_eq!(decode(&buf), Message::Unchoke);
        }

        #[test]
        fn decode_interested() {
            let buf = vec![0, 0, 0, 1, INTERESTED];
            assert_eq!(decode(&buf), Message::Interested);
        }

        #[test]
        fn decode_not_interested() {
            let buf = vec![0, 0, 0, 1, NOT_INTERESTED];
            assert_eq!(decode(&buf), Message::NotInterested);
        }

        #[test]
        fn decode_have() {
            let buf = vec![0, 0, 0, 5, HAVE, 0, 0, 0, 44];
            assert_eq!(decode(&buf), Message::Have { index: 44 });
        }

        #[test]
        fn decode_bitfield() {
            let bitfield = Bitfield(bitvec![Msb0, u8; 0, 0, 1, 0, 1, 0, 0, 0]);
            let buf = vec![0, 0, 0, 2, BITFIELD, 40];
            assert_eq!(decode(&buf), Message::Bitfield { bitfield });
        }

        #[test]
        fn decode_request() {
            let index = 11;
            let begin = 0;
            let length = 2u32.pow(14);

            let mut buf = vec![0, 0, 0, 13];

            buf.push(REQUEST);
            buf.extend_from_slice(&encode_number(index));
            buf.extend_from_slice(&encode_number(begin));
            buf.extend_from_slice(&encode_number(length));

            assert_eq!(
                decode(&buf),
                Message::Request {
                    index,
                    begin,
                    length
                }
            )
        }

        #[test]
        fn decode_piece() {
            let index = 31;
            let begin = 2u32.pow(14);
            let chunk = vec![1; 2usize.pow(14)];
            let cloned_chunk = chunk.clone();

            let expected_index = encode_number(index);
            let expected_begin = encode_number(begin);

            let mut buf = vec![];

            buf.extend_from_slice(&encode_number(1 + 4 + 4 + chunk.len() as u32));
            buf.push(PIECE);
            buf.extend_from_slice(&expected_index);
            buf.extend_from_slice(&expected_begin);
            buf.extend_from_slice(&cloned_chunk);

            assert_eq!(
                decode(&buf),
                Message::Piece {
                    index,
                    begin,
                    chunk
                }
            )
        }

        #[test]
        fn decode_cancel() {
            let index = 11;
            let begin = 0;
            let length = 2u32.pow(14);

            let mut buf = vec![];

            buf.extend_from_slice(&encode_number(1 + 4 + 4 + 4));
            buf.push(CANCEL);
            buf.extend_from_slice(&encode_number(index));
            buf.extend_from_slice(&encode_number(begin));
            buf.extend_from_slice(&encode_number(length));

            assert_eq!(
                decode(&buf),
                Message::Cancel {
                    index,
                    begin,
                    length
                }
            )
        }

        #[test]
        fn decode_handshake() {
            let peer_id = PeerId([
                0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19,
            ]);
            let info_hash = InfoHash([
                0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19,
            ]);

            let mut m = vec![];
            let mut codec = HandshakeCodec;

            m.push(19);
            m.extend_from_slice(BITTORRENT_PROTOCOL);
            m.extend_from_slice(&PROTOCOL_EXTENSION_HEADER);
            m.extend_from_slice(peer_id.as_ref());
            m.extend_from_slice(info_hash.as_ref());

            let mut buf = BytesMut::from(m.as_slice());

            assert_eq!(
                codec.decode(&mut buf).unwrap().unwrap(),
                Handshake {
                    protocol_extension_bytes: PROTOCOL_EXTENSION_HEADER,
                    peer_id,
                    info_hash
                }
            );
        }
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
