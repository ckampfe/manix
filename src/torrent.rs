use sha1::Digest;

use crate::{listener, Port};
use std::{convert::TryInto, fmt::Display, net::IpAddr, path::Path};
use tokio::{select, sync::mpsc};

pub struct TorrentOptions {
    port: Port,
    torrent_channel_buffer_size: usize,
    peer_channel_buffer_size: usize,
}

impl Default for TorrentOptions {
    fn default() -> Self {
        Self {
            port: 6881,
            torrent_channel_buffer_size: 100,
            peer_channel_buffer_size: 20,
        }
    }
}

pub struct Torrent {
    peer_id: String,
    dot_torrent_bencode: nom_bencode::Bencode,
    port: Port,
    uploaded: usize,
    downloaded: usize,
    listener: listener::Listener,
}

impl Torrent {
    pub(crate) fn new(
        options: TorrentOptions,
        dot_torrent_bencode: nom_bencode::Bencode,
    ) -> Result<Self, std::io::Error> {
        let peer_id = generate_peer_id();

        let listener = listener::Listener::new(options.port)?;

        let s = Self {
            peer_id,
            dot_torrent_bencode,
            port: options.port,
            uploaded: 0,
            downloaded: 0,
            listener,
        };

        Ok(s)
    }

    // should always be a hex repr
    pub fn get_info_hash_human(&self) -> String {
        let info_hash = self.get_info_hash_machine();
        hex::encode(info_hash)
    }

    pub fn get_info_hash_machine(&self) -> [u8; 20] {
        match &self.dot_torrent_bencode {
            nom_bencode::Bencode::Dictionary(d) => {
                let info = d.get(&b"info".to_vec()).unwrap();
                let encoded = info.encode();
                let mut hasher = sha1::Sha1::new();
                // process input message
                hasher.update(encoded);

                // acquire hash digest in the form of GenericArray,
                // which in this case is equivalent to [u8; 20]
                let result = hasher.finalize();
                result.try_into().expect("info hash must be 20 bytes")
            }
            _ => panic!(".torrent bencode must be a dictionary"),
        }
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

    pub fn get_peer_id(&self) -> &str {
        self.peer_id.as_ref()
    }

    pub fn get_ip(&self) -> &str {
        todo!()
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
    ) -> Result<nom_bencode::Bencode, Box<dyn std::error::Error>> {
        let uploaded = self.get_uploaded().to_string();
        let downloaded = self.get_downloaded().to_string();
        let info_hash = self.get_info_hash_human();
        let port = self.get_port().to_string();

        let mut params = vec![
            ("info_hash", info_hash.as_str()),
            ("peer_id", self.get_peer_id()),
            ("ip", self.get_ip()),
            ("port", &port),
            ("uploaded", &uploaded),
            ("downloaded", &downloaded),
        ];

        let event_string = event.to_string();

        if event_string != "empty" {
            params.push(("event", &event_string));
        }

        let url = reqwest::Url::parse_with_params(&self.get_announce_url(), params)?;

        let response = reqwest::get(url).await?;
        let bytes = response.bytes().await?;
        let bytes = bytes.to_vec();
        let decoded = nom_bencode::decode(&bytes).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{:?}", e))
        })?;

        Ok(decoded)
    }
}

enum TorrentMsg {
    PeerAccepted,
}

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

pub(crate) fn encode_number(n: u32) -> [u8; 4] {
    n.to_be_bytes()
}

pub(crate) fn decode_number(bytes: [u8; 4]) -> u32 {
    u32::from_be_bytes(bytes)
}

pub(crate) fn generate_peer_id() -> String {
    String::from("foooooooo")
}
