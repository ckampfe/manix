use crate::listener::{self, Listener};
use crate::metainfo::MetaInfo;
use crate::InfoHash;
use crate::{messages, peer_protocol};
use crate::{PeerId, Port};
use rand::Rng;
use sha1::Digest;
use std::collections::HashMap;
use std::convert::{TryFrom, TryInto};
use std::fmt::Display;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::time::Interval;
use tracing::{debug, error, info, instrument};

pub struct TorrentOptions {
    port: Port,
    max_peer_connections: usize,
}

impl Default for TorrentOptions {
    fn default() -> Self {
        Self {
            port: 6881,
            max_peer_connections: 25,
        }
    }
}

#[derive(Debug)]
pub struct Torrent {
    peer_id: PeerId,
    meta_info: MetaInfo,
    torrent_data: PathBuf,
    peers: HashMap<PeerId, tokio::sync::mpsc::Sender<messages::TorrentToPeer>>,
    port: Port,
    uploaded: usize,
    downloaded: usize,
    pieces_bitfield: peer_protocol::Bitfield,
    info_hash: InfoHash,
    listener: Option<Listener<String>>,
    global_max_peer_connections: Arc<tokio::sync::Semaphore>,
    torrent_max_peer_connections: Arc<tokio::sync::Semaphore>,
    peer_to_torrent_tx: tokio::sync::mpsc::Sender<messages::PeerToTorrent>,
    peer_to_torrent_rx: tokio::sync::mpsc::Receiver<messages::PeerToTorrent>,
    event_loop_interrupt_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

// PUBLIC
impl Torrent {
    pub(crate) fn new(
        options: TorrentOptions,
        dot_torrent_bencode: nom_bencode::Bencode,
        torrent_data: PathBuf,
        global_max_peer_connections: Arc<tokio::sync::Semaphore>,
    ) -> Result<Self, std::io::Error> {
        let peer_id = generate_peer_id();
        let info_hash = info_hash(&dot_torrent_bencode);
        let meta_info = MetaInfo::try_from(dot_torrent_bencode)?;
        info!("metainfo reports {} pieces", meta_info.info.pieces.len());
        info!("metainfo reports length of {}", meta_info.info.piece_length);
        let pieces_bitfield = peer_protocol::Bitfield::new(meta_info.info.pieces.len());
        let torrent_max_peer_connections =
            Arc::new(tokio::sync::Semaphore::new(options.max_peer_connections));

        let (peer_to_torrent_tx, peer_to_torrent_rx) = tokio::sync::mpsc::channel(32);

        let peers = HashMap::new();

        Ok(Self {
            peer_id,
            meta_info,
            torrent_data,
            peers,
            port: options.port,
            uploaded: 0,
            downloaded: 0,
            pieces_bitfield,
            info_hash,
            global_max_peer_connections,
            listener: None,
            torrent_max_peer_connections,
            peer_to_torrent_tx,
            peer_to_torrent_rx,
            event_loop_interrupt_tx: None,
        })
    }

    #[instrument(skip(self))]
    pub(crate) async fn start(&mut self) -> Result<(), std::io::Error> {
        self.validate_torrent_data().await?;

        let have_pieces_len = self.pieces_bitfield.count_ones();
        info!("Have {} pieces", have_pieces_len);

        let listener = listener::Listener::new(
            "127.0.0.1:6881".to_string(),
            self.peer_id,
            self.info_hash,
            self.global_max_peer_connections.clone(),
            self.torrent_max_peer_connections.clone(),
            self.peer_to_torrent_tx.clone(),
        );
        self.listener = Some(listener);
        self.listener.as_mut().unwrap().listen().await?;
        let (interrupt_tx, interrupt_rx) = tokio::sync::oneshot::channel();
        self.event_loop_interrupt_tx = Some(interrupt_tx);
        self.enter_event_loop(interrupt_rx).await?;
        Ok(())
    }

    #[instrument(skip(self))]
    pub(crate) async fn pause(&mut self) -> Result<(), std::io::Error> {
        // stop the event loop
        if let Some(tx) = self.event_loop_interrupt_tx.take() {
            tx.send(()).unwrap();
        }

        // drop the listener as well
        let _ = self.listener.take();

        self.announce(AnnounceEvent::Stopped).await?;

        Ok(())
    }

    pub fn get_info_hash_human(&self) -> String {
        let info_hash = self.get_info_hash_machine();
        info_hash.human_readable()
    }

    pub fn get_info_hash_machine(&self) -> InfoHash {
        self.info_hash
    }

    pub fn get_announce_url(&self) -> &str {
        &self.meta_info.announce
    }

    pub fn get_peer_id(&self) -> PeerId {
        self.peer_id
    }

    pub fn get_ip(&self) -> &str {
        "localhost"
    }

    pub fn get_port(&self) -> Port {
        self.port
    }

    pub fn get_uploaded(&self) -> usize {
        self.uploaded
    }

    pub fn get_downloaded(&self) -> usize {
        self.downloaded
    }

    pub fn get_left(&self) -> usize {
        self.meta_info.info.length
    }

    pub fn get_name(&self) -> &str {
        &self.meta_info.info.name
    }

    pub fn get_piece_length(&self) -> usize {
        self.meta_info.info.piece_length
    }

    pub fn get_pieces(&self) -> &[[u8; 20]] {
        &self.meta_info.info.pieces
    }

    #[instrument(skip(self))]
    pub async fn announce(
        &self,
        event: AnnounceEvent,
    ) -> Result<nom_bencode::Bencode, std::io::Error> {
        let uploaded = self.get_uploaded().to_string();
        let downloaded = self.get_downloaded().to_string();
        let port = self.get_port().to_string();
        let peer_id = self.get_peer_id();
        let peer_id = std::str::from_utf8(peer_id.as_ref()).unwrap();
        let info_hash = self.get_info_hash_machine();
        let left = self.get_left().to_string();

        let mut params = vec![
            ("peer_id", peer_id),
            ("ip", self.get_ip()),
            ("port", &port),
            ("uploaded", &uploaded),
            ("downloaded", &downloaded),
            ("left", &left),
        ];

        let event_string = event.to_string();

        if event_string != "empty" {
            params.push(("event", &event_string));
        }

        let url = reqwest::Url::parse_with_params(self.get_announce_url(), params)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()))?;

        // NOTE:
        // This is a hack to manually percent-encode the info_hash,
        // as reqwest and its internal machinery require that URL params be &str's,
        // and provide no way to pass raw bytes.
        // So, we reify the URL to a string, and append the info_hash query param manually.
        let mut url = url.to_string();
        url.push_str("&info_hash=");
        url.push_str(
            &percent_encoding::percent_encode(
                info_hash.as_ref(),
                percent_encoding::NON_ALPHANUMERIC,
            )
            .to_string(),
        );

        // This provides some validation for the above hack, that we correctly made a valid URL
        let url = reqwest::Url::parse(&url)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()))?;

        let response = reqwest::get(url)
            .await
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

        let response_bytes = response
            .bytes()
            .await
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?
            .to_vec();

        let decoded = nom_bencode::decode(&response_bytes).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{:?}", e))
        })?;

        Ok(decoded)
    }

    #[instrument(skip(self))]
    pub async fn validate_torrent_data(&mut self) -> Result<(), std::io::Error> {
        let mut torrent_data_file = tokio::fs::OpenOptions::new()
            .write(true)
            .read(true)
            .create(true)
            .truncate(false)
            .open(&self.torrent_data)
            .await?;

        let file_metadata = torrent_data_file.metadata().await?;

        if file_metadata.len() == 0 {
            torrent_data_file
                .set_len(
                    self.meta_info
                        .info
                        .length
                        .try_into()
                        .expect("File size must be a u64"),
                )
                .await?;
        }

        let buf_len = self.meta_info.info.piece_length;
        let mut buf = vec![0u8; buf_len];
        let piece_hashes = &self.meta_info.info.pieces;
        let piece_hashes_len = piece_hashes.len();
        let mut remaining_length = self.meta_info.info.length;

        assert!(
            piece_hashes_len > 0,
            "There must be more than 0 pieces in a torrent"
        );

        // regular case loop for all but the last piece
        for (i, known_piece_hash) in piece_hashes[..piece_hashes_len - 1].iter().enumerate() {
            torrent_data_file.read_exact(&mut buf).await?;
            remaining_length -= buf_len;
            let bytes_hash = hash(&buf);
            if &bytes_hash == known_piece_hash {
                if let Some(mut bit) = self.pieces_bitfield.get_mut(i) {
                    *bit = true;
                }
            }
        }

        // special case for the last piece, which may be (probably is) shorter than a full piece
        buf.resize(remaining_length, 0u8);
        torrent_data_file.read_exact(&mut buf).await?;
        remaining_length -= buf.len();
        let bytes_hash = hash(&buf);
        if &bytes_hash == piece_hashes.last().unwrap() {
            if let Some(mut bit) = self.pieces_bitfield.last_mut() {
                *bit = true;
            }
        }

        if remaining_length > 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "Torrent ended early",
            ));
        }

        Ok(())
    }
}

// PRIVATE
impl Torrent {
    #[instrument(skip(self))]
    async fn enter_event_loop(
        &mut self,
        mut interrupt_rx: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<(), std::io::Error> {
        let Timers {
            announce_timer,
            debug_timer,
        } = self.start_timers().await;

        tokio::pin!(announce_timer);
        tokio::pin!(debug_timer);

        loop {
            tokio::select! {
                peer_result = self.listener.as_ref().unwrap().accept() => {
                    match peer_result {
                        Ok(peer) => {
                            info!("accepted peer, attempting handshake...");
                            tokio::spawn(async move {
                                peer.enter_event_loop().await
                            });
                        },
                        Err(e) => {
                            error!("{:?}", e);
                            panic!();
                        }
                    }
                }
                message = self.peer_to_torrent_rx.recv() => {
                    match message {
                        Some(message) => match message {
                            messages::PeerToTorrent::Register { remote_peer_id, torrent_to_peer_tx } => {
                                info!("registered remote peer {}", remote_peer_id);
                                self.peers.insert(remote_peer_id, torrent_to_peer_tx);
                            },
                            messages::PeerToTorrent::Deregister { remote_peer_id } => {
                                info!("deregistered remote peer {}", remote_peer_id);
                                self.peers.remove(&remote_peer_id);
                            },
                            messages::PeerToTorrent::RequestBitfield { remote_peer_id: _, responder } => {
                                responder.send(
                                        self.pieces_bitfield.clone()
                                ).map_err(|_e| std::io::Error::new(std::io::ErrorKind::Other, "TODO actually handle this torrent to peer send error"))?;
                            }
                            messages::PeerToTorrent::Bitfield { remote_peer_id, bitfield } => {
                                info!("received bitfield from {}", remote_peer_id);
                                self.pieces_bitfield |= bitfield
                            }
                        },
                        None => error!("peer_to_torrent_rx was closed when torrent.rs tried to receive a message from peer"),
                    }
                }
                _ = announce_timer.tick() => {
                    debug!("ANNOUNCING");
                    self.announce(AnnounceEvent::Empty).await?;
                }
                _ = debug_timer.tick() => {
                    let now = std::time::Instant::now();
                    debug!("DEBUG {:?}", now);
                }
                _ = (&mut interrupt_rx) => {
                    debug!("RECEIVED INTERRUPT");
                    return Ok(());
                }
            }
        }
    }

    async fn start_timers(&self) -> Timers {
        let announce_timer = tokio::time::interval(std::time::Duration::from_secs(60));
        let debug_timer = tokio::time::interval(std::time::Duration::from_secs(5));

        Timers {
            announce_timer,
            debug_timer,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum AnnounceEvent {
    Started,
    Stopped,
    Completed,
    Empty,
}

impl Display for AnnounceEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AnnounceEvent::Started => write!(f, "started"),
            AnnounceEvent::Stopped => write!(f, "stopped"),
            AnnounceEvent::Completed => write!(f, "completed"),
            AnnounceEvent::Empty => write!(f, "empty"),
        }
    }
}

struct Timers {
    announce_timer: Interval,
    debug_timer: Interval,
}

pub(crate) fn generate_peer_id() -> PeerId {
    let rand_string: String = rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(16)
        .map(char::from)
        .collect();

    let mut id = "MNX-".to_string();
    id.push_str(&rand_string);

    let id_bytes: [u8; 20] = id.as_bytes().try_into().unwrap();

    PeerId(id_bytes)
}

fn info_hash(bencode: &nom_bencode::Bencode) -> InfoHash {
    match bencode {
        nom_bencode::Bencode::Dictionary(d) => {
            let info = d.get(&b"info".to_vec()).unwrap();
            let encoded = info.encode();
            InfoHash(hash(&encoded))
        }
        _ => panic!(".torrent bencode must be a dictionary"),
    }
}

fn hash(buf: &[u8]) -> [u8; 20] {
    let mut hasher = sha1::Sha1::new();
    hasher.update(buf);
    hasher
        .finalize()
        .try_into()
        .expect("result must be 20 bytes")
}
