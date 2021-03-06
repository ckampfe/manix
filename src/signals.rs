use crate::peer_protocol;
use crate::PeerId;

#[derive(Debug)]
pub(crate) enum TorrentToPeer {
    Choke,
    NotChoked,
    Interested,
    NotInterested,
    GetPiece(usize),
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
    RequestBitfield {
        remote_peer_id: PeerId,
        responder: tokio::sync::oneshot::Sender<peer_protocol::Bitfield>,
    },
    Bitfield {
        remote_peer_id: PeerId,
        bitfield: peer_protocol::Bitfield,
    },
}
