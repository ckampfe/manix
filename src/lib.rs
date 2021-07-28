use crate::torrent::Torrent;
use std::{collections::BTreeMap, io::Read, path::Path};

mod listener;
mod peer;
pub mod torrent;

type Index = u32;
type Begin = u32;
type Length = u32;
type Port = u16;

pub struct Manix {
    torrents: BTreeMap<String, Torrent>,
}

impl Manix {
    pub fn new() -> Self {
        Self {
            torrents: BTreeMap::new(),
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
        let human_readable_info_hash = torrent.get_info_hash_human();
        if self.torrents.contains_key(&human_readable_info_hash) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!(
                    "{:?} (info_hash {}) already exists",
                    path.as_os_str(),
                    human_readable_info_hash
                ),
            ));
        } else {
            // if not, add it
            let cloned_key = human_readable_info_hash.clone();
            self.torrents.insert(human_readable_info_hash, torrent);
            // return its info hash
            Ok(self.torrents.get(&cloned_key).unwrap())
        }
    }

    pub fn pause_torrent(&mut self, info_hash: &str) {
        todo!()
    }
    pub fn delete_torrent(&mut self, info_hash: &str) {
        todo!()
    }
    pub fn delete_data(&mut self, info_hash: &str) {
        todo!()
    }

    pub fn list_torrents(&self) -> Vec<Torrent> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}
