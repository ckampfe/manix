use crate::peer_protocol;
use crate::{messages, Begin, Index, InfoHash, Length, PeerId};
use futures_util::sink::SinkExt;
use futures_util::StreamExt;
use std::convert::TryInto;
use tokio::net::TcpStream;
use tokio::time::{Interval, MissedTickBehavior};
use tracing::{debug, error, info, instrument, trace};

#[derive(Debug)]
pub(crate) struct PeerOptions {
    pub(crate) socket: TcpStream,
    pub(crate) peer_id: PeerId,
    pub(crate) info_hash: InfoHash,
    pub(crate) peer_to_torrent_tx: tokio::sync::mpsc::Sender<messages::PeerToTorrent>,
    pub(crate) global_permit: tokio::sync::OwnedSemaphorePermit,
    pub(crate) torrent_permit: tokio::sync::OwnedSemaphorePermit,
    pub(crate) piece_length: usize,
    pub(crate) chunk_length: usize,
}

#[derive(Debug)]
pub(crate) struct Peer {
    framed: tokio_util::codec::Framed<tokio::net::TcpStream, peer_protocol::MessageCodec>,
    peer_id: PeerId,
    remote_peer_id: Option<PeerId>,
    info_hash: InfoHash,
    peer_to_torrent_tx: tokio::sync::mpsc::Sender<messages::PeerToTorrent>,
    torrent_to_peer_rx: Option<tokio::sync::mpsc::Receiver<messages::TorrentToPeer>>,
    global_permit: tokio::sync::OwnedSemaphorePermit,
    torrent_permit: tokio::sync::OwnedSemaphorePermit,
    choke_state: ChokeState,
    interest_state: InterestState,
    piece_length: usize,
    chunk_length: usize,
}

impl Peer {
    #[instrument]
    pub(crate) fn new(options: PeerOptions) -> Self {
        let framed = tokio_util::codec::Framed::new(options.socket, peer_protocol::MessageCodec);

        Self {
            framed,
            peer_id: options.peer_id,
            remote_peer_id: None,
            info_hash: options.info_hash,
            peer_to_torrent_tx: options.peer_to_torrent_tx,
            torrent_to_peer_rx: None,
            global_permit: options.global_permit,
            torrent_permit: options.torrent_permit,
            choke_state: ChokeState::Choked,
            interest_state: InterestState::NotInterested,
            piece_length: options.piece_length,
            chunk_length: options.chunk_length,
        }
    }

    #[instrument(skip(self))]
    pub(crate) async fn enter_event_loop(mut self) -> Result<(), std::io::Error> {
        let remote_peer_id = self.handshake_remote_peer().await?;
        self.remote_peer_id = Some(remote_peer_id);

        info!(
            "registering {:?} with owning torrent",
            self.remote_peer_id.as_ref().map(|pid| pid.to_string())
        );
        self.register_with_owning_torrent().await?;

        let current_bitfield = self.request_current_bifield().await?.await.map_err(|_e| {
            std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "TODO handle this oneshot channel error",
            )
        })?;
        info!("got bitfield from owning torrent");

        self.send_bitfield(current_bitfield).await?;
        info!("Sent bitfield to peer");

        self.send_unchoke().await?;
        info!("Sent unchoke to peer");

        self.send_interested().await?;
        info!("Sent interested");

        let Timers { mut keepalive } = self.start_timers().await;

        info!("Started peer timers");

        loop {
            // take ownership of the TorrentToPeer rx channel for the duration of the loop
            let mut rx = self.torrent_to_peer_rx.take().unwrap();

            tokio::select! {
                result = self.receive_message() => {
                    match result {
                        Some(Ok(message)) => {
                            match message {
                                peer_protocol::Message::Keepalive => {
                                    info!("Receieved keepalive from peer")
                                },
                                peer_protocol::Message::Choke => {
                                    info!("Received choke from peer");
                                    self.send_choke().await?
                                },
                                peer_protocol::Message::Unchoke => {
                                    info!("Received unchoke from peer");
                                    self.send_unchoke().await?;
                                },
                                peer_protocol::Message::Interested => {
                                    info!("Received interested from peer");
                                },
                                peer_protocol::Message::NotInterested => {
                                    info!("Received not interested from peer");
                                },
                                peer_protocol::Message::Have { index: _ } => todo!(),
                                peer_protocol::Message::Bitfield { bitfield } => {
                                    info!("Received bitfield from peer");
                                    self.report_bitfield(bitfield).await?;
                                },
                                peer_protocol::Message::Request { index: _, begin: _, length: _ } => todo!(),
                                peer_protocol::Message::Piece { index, begin, chunk: _ } => {
                                    info!("Received piece ({}, {})", index, begin)
                                },
                                peer_protocol::Message::Cancel { index: _, begin: _, length: _ } => todo!(),
                                peer_protocol::Message::Handshake { protocol_extension_bytes: _, peer_id: _, info_hash: _ } => {
                                    error!("shouldn't receive this handshake, but we did!");
                                    todo!("shouldn't receieve this handshake, but we did!")
                                },
                            }
                        }
                        Some(Err(e)) => {
                            error!("{:#?}", e.to_string());
                            self.deregister_with_owning_torrent().await?;
                            return Err(e);
                        }
                        None => todo!("not sure what this branch represents"),
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
                            messages::TorrentToPeer::GetPiece(index) => {
                                let offsets = peer_protocol::chunk_offsets_lengths(self.piece_length, self.chunk_length);
                                for (chunk_offset, chunk_length) in offsets {
                                    self.send_request(
                                        index.try_into().unwrap(),
                                        chunk_offset.try_into().unwrap(),
                                        chunk_length.try_into().unwrap()
                                    ).await?;
                                }
                            }
                        },
                        None => {
                            debug!("Torrent to peer channel closed, disconnecting");
                            return Ok(());
                        },
                    }
                }
                _ = keepalive.tick() => {
                    info!("sent keepalive");
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

        let handshake = self.receive_handshake().await;
        trace!("HANDSHAKE RECEIVED");

        if let Some(message) = handshake
        // if the info hashes match, we can proceed
        // if not, sever the connection and drop the semaphore permit
        {
            let message = message?;
            if let peer_protocol::Message::Handshake {
                peer_id: remote_peer_id,
                info_hash: remote_info_hash,
                ..
            } = message
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
            } else {
                trace!("HANDSHAKE WAS BAD2");
                Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "received a non-handshake message when was expecting handshake",
                ))
            }

            // otherwise if the message is NOT a handshake, it is invalid,
            // so drop the permit and the connection
        } else {
            panic!()
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
    async fn send_bitfield(
        &mut self,
        bitfield: peer_protocol::Bitfield,
    ) -> Result<(), std::io::Error> {
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
        // let bytes: Vec<u8> = message.into();
        // self.socket.write_all(&bytes).await
        // let codec = self.framed.codec_mut();
        // let mut buf = &mut BytesMut::new();
        // codec.encode(message, &mut buf)

        self.framed.send(message).await
    }

    async fn receive_message(&mut self) -> Option<Result<peer_protocol::Message, std::io::Error>> {
        // let message_length = self.receive_length().await?;

        // // TODO: does this need to be fully initialized with 0u8 values
        // // in order for it to be filled by `read_exact`?
        // let mut buf = Vec::with_capacity(message_length as usize);

        // self.socket.read_exact(&mut buf).await?;

        // peer_protocol::Message::try_from(buf)
        //     .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))

        // let codec = self.framed.codec_mut();
        // let mut buf = self.framed.read_buffer_mut();
        // codec.decode(&mut buf)
        self.framed.next().await
    }

    async fn receive_handshake(
        &mut self,
    ) -> Option<Result<peer_protocol::Message, std::io::Error>> {
        // // handshake buf has to be fully filled with bytes, not just allocated capacity with `Vec::with_capacity`.
        // let mut buf = vec![0u8; HANDSHAKE_LENGTH];
        // self.socket.read_exact(&mut buf).await?;

        // peer_protocol::Message::try_from(buf)
        //     .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
        // let codec = self.framed.codec_mut();
        // let mut buf = &mut BytesMut::new();
        // codec.decode(&mut buf)
        self.receive_message().await
    }

    // async fn receive_length(&mut self) -> Result<u32, std::io::Error> {
    //     let mut length_bytes: [u8; 4] = [0; 4];
    //     self.socket.read_exact(&mut length_bytes).await?;
    //     dbg!(length_bytes);
    //     Ok(u32::from_be_bytes(length_bytes))
    // }

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
    async fn request_current_bifield(
        &self,
    ) -> Result<tokio::sync::oneshot::Receiver<peer_protocol::Bitfield>, std::io::Error> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.send_to_owned_torrent(messages::PeerToTorrent::RequestBitfield {
            remote_peer_id: self.remote_peer_id.unwrap(),
            responder: tx,
        })
        .await?;
        Ok(rx)
    }

    #[instrument(skip(self))]
    async fn report_bitfield(
        &self,
        bitfield: peer_protocol::Bitfield,
    ) -> Result<(), std::io::Error> {
        self.send_to_owned_torrent(messages::PeerToTorrent::Bitfield {
            remote_peer_id: self.remote_peer_id.unwrap(),
            bitfield,
        })
        .await?;
        Ok(())
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
