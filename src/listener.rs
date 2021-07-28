use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use crate::peer::Peer;
use crate::Port;

pub(crate) struct Listener {
    port: Port,
    handle: JoinHandle<Result<(), std::io::Error>>,
}

impl Listener {
    pub(crate) fn new(
        port: Port,
        peer_id: [u8; 20],
        info_hash: [u8; 20],
    ) -> Result<Self, std::io::Error> {
        // TODO make this configurable
        let handle = tokio::spawn(async move {
            let listener = TcpListener::bind(format!("127.0.0.1:{}", port)).await?;

            loop {
                let (socket, _socket_addr) = listener.accept().await?;
                // process_socket(socket).await;
                tokio::spawn(async move {
                    let _ = Peer::new(socket, peer_id, info_hash).await;
                });
            }
        });

        Ok(Self { port, handle })
    }
}
