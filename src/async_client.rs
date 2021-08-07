use crate::torrent;
use crate::torrent::Torrent;
use crate::Options;
use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;

pub struct AsyncClient {
    torrents: HashMap<String, Torrent>,
    global_max_peer_connections: Arc<tokio::sync::Semaphore>,
}

impl AsyncClient {
    pub fn new(options: Options) -> Self {
        Self {
            torrents: HashMap::new(),
            global_max_peer_connections: Arc::new(tokio::sync::Semaphore::new(
                options.global_max_peer_connections,
            )),
        }
    }

    pub async fn add_torrent<R: Read>(
        &mut self,
        mut dot_torrent_read: R,
        torrent_data: PathBuf,
    ) -> Result<&Torrent, std::io::Error> {
        // read the .torrent
        let mut buf = vec![];
        dot_torrent_read.read_to_end(&mut buf)?;

        // decode .torrent file
        let dot_torrent_bencode = nom_bencode::decode(&buf).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{:?}", e))
        })?;
        // validate it
        let options = torrent::TorrentOptions::default();
        let torrent = Torrent::new(
            options,
            dot_torrent_bencode,
            torrent_data,
            self.global_max_peer_connections.clone(),
        )?;
        if let std::collections::hash_map::Entry::Vacant(e) =
            self.torrents.entry(torrent.get_info_hash_human())
        {
            // if not, add it and return a reference to its Torrent
            Ok(e.insert(torrent))
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!(
                    "(info_hash {}) already exists",
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

    pub async fn delete_torrent(&mut self, info_hash: &str) -> Option<Torrent> {
        self.torrents.remove(info_hash)
    }

    pub async fn delete_data(&mut self, _info_hash: &str) {
        todo!()
    }

    pub async fn list_torrents(&self) -> Vec<&Torrent> {
        self.torrents.values().collect()
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
