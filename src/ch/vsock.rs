use std::io;
use std::path::Path;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

/// The longest handshake reply read.
const MAX_REPLY: usize = 64;

/// Connects to a guest's vsock `port` through cloud-hypervisor's host-side
/// socket, which takes a `CONNECT <port>` line first and answers
/// `OK <host port>`. The handshake must finish within `timeout`; nothing
/// the guest sends after it is consumed.
pub async fn dial_guest(vsock_socket: impl AsRef<Path>, port: u32, timeout: Duration) -> io::Result<UnixStream> {
    let handshake = async {
        let mut conn = UnixStream::connect(vsock_socket).await?;
        conn.write_all(format!("CONNECT {port}\n").as_bytes()).await?;
        // Byte by byte, so nothing the guest sends after the reply is
        // buffered away.
        let mut line = Vec::with_capacity(MAX_REPLY);
        while line.len() < MAX_REPLY {
            match conn.read_u8().await? {
                b'\n' => break,
                b => line.push(b),
            }
        }
        if !line.starts_with(b"OK ") {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                format!("{:?}", String::from_utf8_lossy(&line)),
            ));
        }
        Ok(conn)
    };
    tokio::time::timeout(timeout, handshake)
        .await
        .map_err(|_| {
            io::Error::new(io::ErrorKind::TimedOut, format!("vsock port {port}: no handshake within {timeout:?}"))
        })?
        .map_err(|e| io::Error::new(e.kind(), format!("vsock port {port}: {e}")))
}

/// Where cloud-hypervisor forwards a guest's connections to host `port`:
/// the vsock socket's path with `_<port>` appended. Whoever listens there
/// is reached by that guest only.
#[must_use]
pub fn guest_port_socket(vsock_socket: &str, port: u32) -> String {
    format!("{vsock_socket}_{port}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::net::UnixListener;

    /// Answers the handshake like cloud-hypervisor: OK for port 5002, an
    /// error line otherwise, then sends one line; silent for port 7.
    fn fake_vsock() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("vsock.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let mut r = BufReader::new(stream);
                    let mut line = String::new();
                    r.read_line(&mut line).await.unwrap();
                    let mut stream = r.into_inner();
                    match line.as_str() {
                        // The reply and the guest's first bytes arrive
                        // together.
                        "CONNECT 5002\n" => stream.write_all(b"OK 1073741824\nhello\n").await.unwrap(),
                        "CONNECT 7\n" => tokio::time::sleep(Duration::from_secs(60)).await,
                        _ => stream.write_all(b"refused\n").await.unwrap(),
                    }
                });
            }
        });
        (dir, socket)
    }

    #[tokio::test]
    async fn dials_a_guest() {
        let (_dir, socket) = fake_vsock();
        let conn = dial_guest(&socket, 5002, Duration::from_secs(2)).await.unwrap();
        let mut line = String::new();
        BufReader::new(conn).read_line(&mut line).await.unwrap();
        assert_eq!(line, "hello\n", "guest data after the handshake");
        let err = dial_guest(&socket, 9, Duration::from_secs(2)).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::ConnectionRefused, "{err}");
        let err = dial_guest(&socket, 7, Duration::from_millis(100)).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut, "{err}");
    }

    #[test]
    fn port_socket() {
        assert_eq!(guest_port_socket("/run/vm/vsock.sock", 5001), "/run/vm/vsock.sock_5001");
    }
}
