use crate::async_client::AsyncClient;
use crate::torrent::Torrent;
use crate::Options;
use std::io::Read;
use std::path::PathBuf;

pub struct BlockingClient {
    inner: AsyncClient,
    rt: tokio::runtime::Runtime,
}

impl BlockingClient {
    pub fn new(options: Options) -> Self {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        Self {
            inner: AsyncClient::new(options),
            rt,
        }
    }

    pub fn add_torrent<R: Read>(
        &mut self,
        dot_torrent_read: R,
        torrent_data: PathBuf,
    ) -> Result<&Torrent, std::io::Error> {
        self.rt
            .block_on(self.inner.add_torrent(dot_torrent_read, torrent_data))
    }

    pub fn start_torrent(&mut self, info_hash: &str) -> Result<(), std::io::Error> {
        self.rt.block_on(self.inner.start_torrent(info_hash))
    }

    pub fn pause_torrent(&mut self, info_hash: &str) -> Result<(), std::io::Error> {
        self.rt.block_on(self.inner.pause_torrent(info_hash))
    }

    pub fn list_torrents(&self) -> Vec<&Torrent> {
        self.rt.block_on(self.inner.list_torrents())
    }

    // pub fn get_torrents_hash(&self) -> [u8; 20] {
    //     self.rt.block_on(self.inner.get_torrents_hash())
    // }
}
