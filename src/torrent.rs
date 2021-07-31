use sha1::Digest;

use crate::{listener, peer, InfoHash, PeerId, Port};
use std::{
    convert::TryInto,
    fmt::Display,
    io::{Read, Write},
    sync::Arc,
};

pub struct TorrentOptions {
    port: Port,
    torrent_channel_buffer_size: usize,
    max_peer_connections: usize,
}

impl Default for TorrentOptions {
    fn default() -> Self {
        Self {
            port: 6881,
            torrent_channel_buffer_size: 100,
            max_peer_connections: 25,
        }
    }
}

pub struct Torrent {
    dot_torrent_bencode: nom_bencode::Bencode,
    torrent_data: Box<dyn crate::ReadWrite>,
    port: Port,
    uploaded: usize,
    downloaded: usize,
    peer_id: PeerId,
    info_hash: InfoHash,
    listener: Option<listener::Listener>,
    global_max_peer_connections: Arc<tokio::sync::Semaphore>,
    torrent_max_peer_connections: Arc<tokio::sync::Semaphore>,
    peer_to_torrent_tx: tokio::sync::mpsc::Sender<peer::PeerToTorrent>,
    peer_to_torrent_rx: tokio::sync::mpsc::Receiver<peer::PeerToTorrent>,
}

impl Torrent {
    pub(crate) fn new(
        options: TorrentOptions,
        dot_torrent_bencode: nom_bencode::Bencode,
        torrent_data: Box<dyn crate::ReadWrite>,
        global_max_peer_connections: Arc<tokio::sync::Semaphore>,
    ) -> Result<Self, std::io::Error> {
        let peer_id = generate_peer_id();
        let info_hash = info_hash(&dot_torrent_bencode);
        let torrent_max_peer_connections =
            Arc::new(tokio::sync::Semaphore::new(options.max_peer_connections));

        let (peer_to_torrent_tx, peer_to_torrent_rx) = tokio::sync::mpsc::channel(32);

        Ok(Self {
            peer_id,
            dot_torrent_bencode,
            torrent_data,
            port: options.port,
            uploaded: 0,
            downloaded: 0,
            info_hash,
            listener: None,
            global_max_peer_connections,
            torrent_max_peer_connections,
            peer_to_torrent_tx,
            peer_to_torrent_rx,
        })
    }

    pub(crate) async fn start(&mut self) -> Result<(), std::io::Error> {
        let listener = listener::Listener::new(
            self.port,
            self.peer_id,
            self.info_hash,
            self.global_max_peer_connections.clone(),
            self.torrent_max_peer_connections.clone(),
            self.peer_to_torrent_tx.clone(),
        )?;
        self.listener = Some(listener);
        self.announce(AnnounceEvent::Started).await?;
        Ok(())
    }

    pub(crate) async fn pause(&mut self) -> Result<(), std::io::Error> {
        self.listener = None;
        Ok(())
    }

    pub fn get_info_hash_human(&self) -> String {
        let info_hash = self.get_info_hash_machine();
        info_hash.human_readable()
    }

    pub fn get_info_hash_machine(&self) -> InfoHash {
        self.info_hash
    }

    pub fn get_announce_url(&self) -> String {
        match &self.dot_torrent_bencode {
            nom_bencode::Bencode::Dictionary(d) => {
                let announce = d.get(&b"announce".to_vec()).unwrap();
                match announce {
                    nom_bencode::Bencode::String(s) => std::str::from_utf8(s).unwrap().to_owned(),
                    _ => panic!("announce was not a string"),
                }
            }
            _ => panic!(".torrent was not a dict"),
        }
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

        let mut params = vec![
            ("peer_id", peer_id),
            ("ip", self.get_ip()),
            ("port", &port),
            ("uploaded", &uploaded),
            ("downloaded", &downloaded),
        ];

        let event_string = event.to_string();

        if event_string != "empty" {
            params.push(("event", &event_string));
        }

        let url = reqwest::Url::parse_with_params(&self.get_announce_url(), params)
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

        println!("RESPONSE: {}", &decoded.to_string());

        match &decoded {
            nom_bencode::Bencode::Dictionary(d) => {
                for (k, v) in d {
                    println!("{}", std::str::from_utf8(k).unwrap());
                    // match v {
                    //     nom_bencode::Bencode::String(s) => {
                    //         println!(
                    //             "{}: {:?}",
                    //             std::str::from_utf8(&k).unwrap(),
                    //             std::str::from_utf8(&s).unwrap()
                    //         );
                    //     }
                    //     _ => panic!(),
                    // }
                }
            }
            _ => panic!(),
        }

        Ok(decoded)
    }
}

enum TorrentToPeer {}

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

pub(crate) fn generate_peer_id() -> PeerId {
    let x: &[u8] = b"foooooooo00000000000";
    PeerId(x.try_into().unwrap())
}

pub fn info_hash(bencode: &nom_bencode::Bencode) -> InfoHash {
    match bencode {
        nom_bencode::Bencode::Dictionary(d) => {
            let info = d.get(&b"info".to_vec()).unwrap();
            let encoded = info.encode();
            let mut hasher = sha1::Sha1::new();
            // process input message
            hasher.update(encoded);

            // acquire hash digest in the form of GenericArray,
            // which in this case is equivalent to [u8; 20]
            let result = hasher.finalize();
            InfoHash(result.try_into().expect("info hash must be 20 bytes"))
        }
        _ => panic!(".torrent bencode must be a dictionary"),
    }
}
