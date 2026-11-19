//! The guest side of virtio-vsock: connecting to the host and listening
//! for it.

use std::io;

use tokio_vsock::{VMADDR_CID_ANY, VsockAddr};
pub use tokio_vsock::{VsockListener, VsockStream};

/// The host's address as seen from a guest.
pub const CID_HOST: u32 = tokio_vsock::VMADDR_CID_HOST;

/// Connects from a guest to `port` on `cid`, usually [`CID_HOST`].
pub async fn dial(cid: u32, port: u32) -> io::Result<VsockStream> {
    VsockStream::connect(VsockAddr::new(cid, port))
        .await
        .map_err(|e| io::Error::new(e.kind(), format!("vsock connect {cid}:{port}: {e}")))
}

/// Listens on `port` for connections from the host.
pub fn listen(port: u32) -> io::Result<VsockListener> {
    VsockListener::bind(VsockAddr::new(VMADDR_CID_ANY, port))
        .map_err(|e| io::Error::new(e.kind(), format!("vsock bind {port}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // Needs the vsock loopback transport (vsock_loopback), which build
    // sandboxes and many hosts lack; then there is nothing to test.
    #[tokio::test]
    async fn loopback() {
        let Ok(listener) = listen(0x7a3c) else { return };
        let Ok(mut conn) = dial(tokio_vsock::VMADDR_CID_LOCAL, 0x7a3c).await else { return };
        let (mut accepted, _) = listener.accept().await.unwrap();
        conn.write_all(b"ping").await.unwrap();
        let mut buf = [0; 4];
        accepted.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");
    }
}
