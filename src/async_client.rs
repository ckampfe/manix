use std::{collections::BTreeMap, io::Read, sync::Arc};

use sha1::Digest;

use crate::{
    torrent::{self, Torrent},
    Options, ReadWrite,
};

pub struct AsyncClient {
    torrents: BTreeMap<String, Torrent>,
    global_max_peer_connections: Arc<tokio::sync::Semaphore>,
}

impl AsyncClient {
    pub fn new(options: Options) -> Self {
        Self {
            torrents: BTreeMap::new(),
            global_max_peer_connections: Arc::new(tokio::sync::Semaphore::new(
                options.global_max_peer_connections,
            )),
        }
    }

    // pub async fn add_torrent<R: Read>(
    //     &mut self,
    //     dot_torrent_read: R,
    //     torrent_data: Box<dyn ReadWrite>,
    // ) -> Result<&Torrent, std::io::Error> {
    //     let t = self.add_torrent_impl(dot_torrent_read, torrent_data)?;
    //     self.update_torrents_hash().await;
    //     self.get_torrent(&t)
    // }

    pub async fn add_torrent<R: Read>(
        &mut self,
        mut dot_torrent_read: R,
        torrent_data: Box<dyn ReadWrite>,
    ) -> Result<&Torrent, std::io::Error> {
        // // open .torrent file
        // let mut dot_torrent = std::fs::File::open(dot_torrent_path)?;

        // // read .torrent file
        // let mut buf = vec![];
        // dot_torrent.read_to_end(&mut buf)?;

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
        // has it already been loaded?

        // let human_hash = torrent.get_info_hash_human();

        if let std::collections::btree_map::Entry::Vacant(e) =
            self.torrents.entry(torrent.get_info_hash_human())
        {
            // if not, add it and return a reference to its Torrent
            // let t = e.insert(torrent);
            // r.update_torrents_hash();
            Ok(e.insert(torrent))
            // Ok(human_hash)
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

    pub async fn delete_data(&mut self, info_hash: &str) {
        todo!()
    }

    pub async fn list_torrents(&self) -> Vec<&Torrent> {
        self.torrents.values().collect()
    }

    // async fn update_torrents_hash(&mut self) {
    //     let hash = self.hash();
    //     self.torrents_hash = hash;
    // }

    // pub async fn get_torrents_hash(&self) -> [u8; 20] {
    //     self.torrents_hash
    // }

    // fn hash(&self) -> [u8; 20] {
    //     let mut hasher = sha1::Sha1::new();

    //     for key in self.torrents.keys() {
    //         hasher.update(key);
    //     }

    //     // acquire hash digest in the form of GenericArray,
    //     // which in this case is equivalent to [u8; 20]
    //     let result = hasher.finalize();
    //     result.into()
    // }

    // fn get_torrent(&mut self, info_hash: &str) -> Result<&Torrent, std::io::Error> {
    //     let torrent = self.torrents.get(info_hash);
    //     if let Some(torrent) = torrent {
    //         Ok(torrent)
    //     } else {
    //         Err(std::io::Error::new(
    //             std::io::ErrorKind::NotFound,
    //             format!("Could not find torrent for info hash {}", info_hash),
    //         ))
    //     }
    // }

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
