use crate::torrent::Torrent;
use std::{collections::BTreeMap, io::Read, path::Path, sync::Arc};

mod listener;
mod peer;
mod peer_protocol;
pub mod torrent;

type Index = u32;
type Begin = u32;
type Length = u32;
type Port = u16;

pub struct Manix {
    torrents: BTreeMap<String, Torrent>,
    max_peer_connections: Arc<tokio::sync::Semaphore>,
}

impl Manix {
    pub fn new(options: ManixOptions) -> Self {
        Self {
            torrents: BTreeMap::new(),
            max_peer_connections: Arc::new(tokio::sync::Semaphore::new(
                options.max_peer_connections,
            )),
        }
    }

    pub fn add_torrent(&mut self, path: &Path) -> Result<&Torrent, std::io::Error> {
        // open .torrent file
        let mut dot_torrent = std::fs::File::open(path)?;

        // read .torrent file
        let mut buf = vec![];
        dot_torrent.read_to_end(&mut buf)?;

        // decode .torrent file
        let dot_torrent_bencode = nom_bencode::decode(&buf).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{:?}", e))
        })?;
        // validate it
        let options = torrent::TorrentOptions::default();
        let torrent = Torrent::new(options, dot_torrent_bencode)?;
        // has it already been loaded?
        if let std::collections::btree_map::Entry::Vacant(e) =
            self.torrents.entry(torrent.get_info_hash_human())
        {
            // if not, add it and return a reference to its Torrent
            Ok(e.insert(torrent))
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!(
                    "{:?} (info_hash {}) already exists",
                    path.as_os_str(),
                    torrent.get_info_hash_human()
                ),
            ))
        }
    }

    pub async fn start_torrent(&mut self, info_hash: &str) -> Result<(), std::io::Error> {
        let torrent = self.get_torrent_mut(info_hash)?;
        torrent.start().await
    }

    pub async fn pause_torrent(&mut self, info_hash: &str) -> Result<(), std::io::Error> {
        let torrent = self.get_torrent_mut(info_hash)?;
        torrent.pause().await
    }

    pub fn delete_torrent(&mut self, info_hash: &str) -> Option<Torrent> {
        self.torrents.remove(info_hash)
    }

    pub fn delete_data(&mut self, info_hash: &str) {
        todo!()
    }

    pub fn list_torrents(&self) -> Vec<Torrent> {
        todo!()
    }

    fn get_torrent_mut(&mut self, info_hash: &str) -> Result<&mut Torrent, std::io::Error> {
        let torrent = self.torrents.get_mut(info_hash);
        if let Some(torrent) = torrent {
            Ok(torrent)
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Could not find torrent for info hash {}", info_hash),
            ))
        }
    }
}

pub struct ManixOptions {
    max_peer_connections: usize,
}

impl Default for ManixOptions {
    fn default() -> Self {
        Self {
            max_peer_connections: 500,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PeerId([u8; 20]);

impl From<[u8; 20]> for PeerId {
    fn from(bytes: [u8; 20]) -> Self {
        Self(bytes)
    }
}

impl AsRef<[u8]> for PeerId {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InfoHash([u8; 20]);

impl From<[u8; 20]> for InfoHash {
    fn from(bytes: [u8; 20]) -> Self {
        Self(bytes)
    }
}

impl AsRef<[u8]> for InfoHash {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}
