use crate::PeerId;

pub(crate) enum TorrentToPeer {
    Choke,
    NotChoked,
    Interested,
    NotInterested,
}

#[derive(Debug)]
pub(crate) enum PeerToTorrent {
    Register {
        remote_peer_id: PeerId,
        torrent_to_peer_tx: tokio::sync::mpsc::Sender<TorrentToPeer>,
    },
    Deregister {
        remote_peer_id: PeerId,
    },
}
