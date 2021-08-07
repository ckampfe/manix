use async_client::AsyncClient;
use blocking_client::BlockingClient;
use std::fmt::Display;

pub mod async_client;
pub mod blocking_client;
mod listener;
mod metainfo;
mod peer;
mod peer_protocol;
pub mod torrent;

type Index = u32;
type Begin = u32;
type Length = u32;
type Port = u16;

pub fn async_client(options: Options) -> AsyncClient {
    AsyncClient::new(options)
}

pub fn blocking_client(options: Options) -> BlockingClient {
    BlockingClient::new(options)
}

pub struct Options {
    global_max_peer_connections: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            global_max_peer_connections: 500,
        }
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct PeerId([u8; 20]);

impl PeerId {
    pub fn human_readable(&self) -> String {
        self.to_string()
    }
}

impl Display for PeerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(
            f,
            "{}",
            std::str::from_utf8(&self.0)
                .expect("peer ids must be valid UTF-8")
                .to_string()
        )
    }
}

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

impl InfoHash {
    pub fn human_readable(&self) -> String {
        hex::encode(self.as_ref())
    }
}

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
