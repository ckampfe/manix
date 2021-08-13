use crate::peer_protocol;
use crate::{messages, Begin, Index, InfoHash, Length, PeerId};
use futures_util::sink::SinkExt;
use futures_util::StreamExt;
use std::convert::TryInto;
use tokio::time::{Interval, MissedTickBehavior};
use tracing::{debug, error, info, instrument};

#[derive(Debug)]
pub(crate) struct PeerOptions {
    pub(crate) connection:
        tokio_util::codec::Framed<tokio::net::TcpStream, peer_protocol::MessageCodec>,
    pub(crate) peer_id: PeerId,
    pub(crate) remote_peer_id: PeerId,
    pub(crate) info_hash: InfoHash,
    pub(crate) peer_to_torrent_tx: tokio::sync::mpsc::Sender<messages::PeerToTorrent>,
    pub(crate) torrent_to_peer_rx: tokio::sync::mpsc::Receiver<messages::TorrentToPeer>,
    pub(crate) global_permit: tokio::sync::OwnedSemaphorePermit,
    pub(crate) torrent_permit: tokio::sync::OwnedSemaphorePermit,
    pub(crate) piece_length: usize,
    pub(crate) chunk_length: usize,
    pub(crate) choke_state: ChokeState,
    pub(crate) interest_state: InterestState,
}

#[derive(Debug)]
pub(crate) struct Peer {
    connection: tokio_util::codec::Framed<tokio::net::TcpStream, peer_protocol::MessageCodec>,
    peer_id: PeerId,
    remote_peer_id: PeerId,
    info_hash: InfoHash,
    peer_to_torrent_tx: tokio::sync::mpsc::Sender<messages::PeerToTorrent>,
    torrent_to_peer_rx: tokio::sync::mpsc::Receiver<messages::TorrentToPeer>,
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
        Self {
            connection: options.connection,
            peer_id: options.peer_id,
            remote_peer_id: options.remote_peer_id,
            info_hash: options.info_hash,
            peer_to_torrent_tx: options.peer_to_torrent_tx,
            torrent_to_peer_rx: options.torrent_to_peer_rx,
            global_permit: options.global_permit,
            torrent_permit: options.torrent_permit,
            choke_state: ChokeState::Choked,
            interest_state: InterestState::NotInterested,
            piece_length: options.piece_length,
            chunk_length: options.chunk_length,
        }
    }

    #[instrument(skip(self))]
    pub(crate) async fn event_loop(mut self) -> Result<(), std::io::Error> {
        info!(
            "registering remote peer id {:?} with owning torrent",
            self.remote_peer_id.to_string()
        );

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
            // let mut rx = &mut self.torrent_to_peer_rx;

            tokio::select! {
                result = Peer::receive_message(&mut self.connection) => {
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
                            }
                        }
                        Some(Err(e)) => {
                            error!("{:#?}", e.to_string());
                            self.deregister_with_owning_torrent().await?;
                            return Err(e);
                        }
                        None => {
                            info!("remote peer hung up");
                            self.deregister_with_owning_torrent().await?;
                            return Ok(())
                        },
                    }
                }
                result = Peer::receive_message_from_owning_torrent(&mut self.torrent_to_peer_rx) => {
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
        }
    }

    #[instrument(skip(self))]
    async fn start_timers(&self) -> Timers {
        let mut keepalive = tokio::time::interval(std::time::Duration::from_secs(30));
        keepalive.set_missed_tick_behavior(MissedTickBehavior::Burst);

        Timers { keepalive }
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
        self.connection.send(message).await?;
        self.connection.flush().await
    }

    async fn receive_message(
        connection: &mut tokio_util::codec::Framed<
            tokio::net::TcpStream,
            peer_protocol::MessageCodec,
        >,
    ) -> Option<Result<peer_protocol::Message, std::io::Error>> {
        connection.next().await
    }

    #[instrument(skip(self))]
    async fn deregister_with_owning_torrent(&mut self) -> Result<(), std::io::Error> {
        self.send_to_owned_torrent(messages::PeerToTorrent::Deregister {
            remote_peer_id: self.remote_peer_id,
        })
        .await
    }

    #[instrument(skip(self))]
    async fn request_current_bifield(
        &self,
    ) -> Result<tokio::sync::oneshot::Receiver<peer_protocol::Bitfield>, std::io::Error> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.send_to_owned_torrent(messages::PeerToTorrent::RequestBitfield {
            remote_peer_id: self.remote_peer_id,
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
            remote_peer_id: self.remote_peer_id,
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

    #[instrument(skip(rx))]
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
pub(crate) enum ChokeState {
    Choked,
    NotChoked,
}

#[derive(Debug)]
pub(crate) enum InterestState {
    Interested,
    NotInterested,
}
