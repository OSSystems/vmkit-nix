use std::path::PathBuf;
use std::time::Duration;

use http_body_util::{BodyExt, Empty, Full, Limited};
use hyper::body::Bytes;
use hyper::header::{CONNECTION, CONTENT_TYPE, HOST};
use hyper::{Method, Request, StatusCode};
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use tokio::net::UnixStream;

/// How long one API call may take.
const TIMEOUT: Duration = Duration::from_secs(5);
/// The most of a response vmkit reads.
const MAX_RESPONSE: usize = 4 << 20;

/// Talks to one cloud-hypervisor's REST API over its Unix socket.
///
/// Every call opens its own connection and closes it before returning:
/// idle keep-alive connections would pile up against the VMM's small
/// connection limit.
#[derive(Debug, Clone)]
pub struct Client {
    socket: PathBuf,
}

/// A failed API call.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The socket could not be reached or the connection broke.
    #[error("cloud-hypervisor {endpoint}: {source}")]
    Io {
        /// The endpoint called.
        endpoint: &'static str,
        /// What went wrong.
        source: std::io::Error,
    },
    /// The exchange was not valid HTTP, or the response was too large.
    #[error("cloud-hypervisor {endpoint}: {reason}")]
    Protocol {
        /// The endpoint called.
        endpoint: &'static str,
        /// What went wrong.
        reason: String,
    },
    /// The VMM answered with an error.
    #[error("cloud-hypervisor {endpoint}: {status}: {body}")]
    Status {
        /// The endpoint called.
        endpoint: &'static str,
        /// The HTTP status.
        status: StatusCode,
        /// The VMM's explanation.
        body: String,
    },
    /// The call did not finish in time.
    #[error("cloud-hypervisor {endpoint}: no answer within {TIMEOUT:?}")]
    Timeout {
        /// The endpoint called.
        endpoint: &'static str,
    },
}

/// The part of `vm.info` vmkit reads.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct VmInfo {
    /// `Created`, `Running`, `Shutdown`, `Paused`…
    pub state: String,
    /// Boot memory plus what virtio-mem has plugged, in bytes; 0 when the
    /// VMM does not say.
    #[serde(default)]
    pub memory_actual_size: i64,
}

impl Client {
    /// Talks to the API socket at `socket`.
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Client { socket: socket.into() }
    }

    /// The VM's state and memory size.
    pub async fn info(&self) -> Result<VmInfo, Error> {
        let endpoint = "vm.info";
        let body = self.call(Method::GET, endpoint, None).await?;
        serde_json::from_slice(&body).map_err(|e| Error::Protocol { endpoint, reason: e.to_string() })
    }

    /// Asks for `desired_bytes` of guest RAM; virtio-mem plugs or unplugs
    /// the difference above the boot size.
    pub async fn resize_memory(&self, desired_bytes: i64) -> Result<(), Error> {
        let body = format!(r#"{{"desired_ram":{desired_bytes}}}"#).into_bytes();
        self.call(Method::PUT, "vm.resize", Some(body)).await.map(drop)
    }

    /// Presses the guest's ACPI power button.
    pub async fn power_button(&self) -> Result<(), Error> {
        self.call(Method::PUT, "vm.power-button", None).await.map(drop)
    }

    async fn call(&self, method: Method, endpoint: &'static str, body: Option<Vec<u8>>) -> Result<Bytes, Error> {
        tokio::time::timeout(TIMEOUT, self.exchange(method, endpoint, body))
            .await
            .map_err(|_| Error::Timeout { endpoint })?
    }

    async fn exchange(&self, method: Method, endpoint: &'static str, body: Option<Vec<u8>>) -> Result<Bytes, Error> {
        let protocol = |e: hyper::Error| Error::Protocol { endpoint, reason: e.to_string() };
        let stream = UnixStream::connect(&self.socket).await.map_err(|source| Error::Io { endpoint, source })?;
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream)).await.map_err(protocol)?;

        let request = Request::builder()
            .method(method)
            .uri(format!("/api/v1/{endpoint}"))
            .header(HOST, "localhost")
            .header(CONNECTION, "close");
        let request = match body {
            Some(body) => request.header(CONTENT_TYPE, "application/json").body(Full::new(Bytes::from(body)).boxed()),
            None => request.body(Empty::new().map_err(|never| match never {}).boxed()),
        }
        .expect("a valid request");

        // The connection is driven here, not spawned, so it is closed by
        // the time the call returns.
        let exchange = async move {
            let response = sender.send_request(request).await.map_err(protocol)?;
            let status = response.status();
            let body = Limited::new(response.into_body(), MAX_RESPONSE)
                .collect()
                .await
                .map_err(|e| Error::Protocol { endpoint, reason: e.to_string() })?
                .to_bytes();
            Ok::<_, Error>((status, body))
        };
        let (result, _) = tokio::join!(exchange, conn);
        let (status, body) = result?;
        if !status.is_success() {
            let body = String::from_utf8_lossy(&body).trim().to_owned();
            return Err(Error::Status { endpoint, status, body });
        }
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    #[derive(Debug, Clone, PartialEq)]
    struct ApiCall {
        method: String,
        path: String,
        content_type: Option<String>,
        body: Option<serde_json::Value>,
    }

    struct FakeCh {
        _dir: tempfile::TempDir,
        socket: PathBuf,
        calls: Arc<Mutex<Vec<ApiCall>>>,
        open: Arc<AtomicI64>,
    }

    /// Serves the cloud-hypervisor API on a Unix socket like its HTTP
    /// server: one request per connection, answered with the status line
    /// and body `respond` picks, then the connection is closed. Records the
    /// calls and counts the connections still open.
    fn fake_ch(respond: fn(&ApiCall) -> (&'static str, &'static str)) -> FakeCh {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("api.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let open = Arc::new(AtomicI64::new(0));
        let (calls2, open2) = (calls.clone(), open.clone());
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                open2.fetch_add(1, Ordering::SeqCst);
                let (calls, open) = (calls2.clone(), open2.clone());
                tokio::spawn(async move {
                    let mut r = BufReader::new(stream);
                    let mut line = String::new();
                    r.read_line(&mut line).await.unwrap();
                    let mut parts = line.split_whitespace();
                    let (method, path) = (parts.next().unwrap().to_owned(), parts.next().unwrap().to_owned());
                    let (mut length, mut content_type) = (0, None);
                    loop {
                        let mut header = String::new();
                        r.read_line(&mut header).await.unwrap();
                        let header = header.trim_end();
                        if header.is_empty() {
                            break;
                        }
                        let (name, value) = header.split_once(':').unwrap();
                        match name.to_ascii_lowercase().as_str() {
                            "content-length" => length = value.trim().parse().unwrap(),
                            "content-type" => content_type = Some(value.trim().to_owned()),
                            _ => {}
                        }
                    }
                    let mut raw = vec![0; length];
                    r.read_exact(&mut raw).await.unwrap();
                    let body = (length > 0).then(|| serde_json::from_slice(&raw).unwrap());
                    let call = ApiCall { method, path, content_type, body };
                    let (status, body) = respond(&call);
                    calls.lock().unwrap().push(call);
                    let mut stream = r.into_inner();
                    let response = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\n\r\n{body}", body.len());
                    stream.write_all(response.as_bytes()).await.unwrap();
                    // Like the VMM, wait for the client to hang up.
                    let mut rest = Vec::new();
                    let _ = stream.read_to_end(&mut rest).await;
                    open.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        FakeCh { _dir: dir, socket, calls, open }
    }

    #[tokio::test]
    async fn request_shapes() {
        let ch = fake_ch(|call| {
            if call.path == "/api/v1/vm.info" {
                (
                    "200 OK",
                    r#"{"state":"Running","config":{"cpus":{"boot_vcpus":16},"memory":{"size":2147483648}},"memory_actual_size":3221225472}"#,
                )
            } else {
                ("204 No Content", "")
            }
        });
        let client = Client::new(&ch.socket);
        let info = client.info().await.unwrap();
        assert_eq!(info, VmInfo { state: "Running".into(), memory_actual_size: 3 << 30 });
        client.resize_memory(3 << 30).await.unwrap();
        client.power_button().await.unwrap();
        let calls = ch.calls.lock().unwrap().clone();
        assert_eq!(
            calls,
            [
                ApiCall { method: "GET".into(), path: "/api/v1/vm.info".into(), content_type: None, body: None },
                ApiCall {
                    method: "PUT".into(),
                    path: "/api/v1/vm.resize".into(),
                    content_type: Some("application/json".into()),
                    body: Some(serde_json::json!({"desired_ram": 3_i64 << 30})),
                },
                ApiCall {
                    method: "PUT".into(),
                    path: "/api/v1/vm.power-button".into(),
                    content_type: None,
                    body: None
                },
            ]
        );
    }

    #[tokio::test]
    async fn reports_api_errors() {
        let ch = fake_ch(|_| ("500 Internal Server Error", "Error from API: The VM could not be resized"));
        let err = Client::new(&ch.socket).resize_memory(4 << 30).await.unwrap_err();
        assert!(matches!(err, Error::Status { status: StatusCode::INTERNAL_SERVER_ERROR, .. }), "{err:?}");
        assert!(err.to_string().contains("could not be resized"), "{err}");
        let err = Client::new(ch.socket.with_file_name("missing.sock")).info().await.unwrap_err();
        assert!(matches!(err, Error::Io { endpoint: "vm.info", .. }), "{err:?}");
    }

    // The memory loop calls vm.info on every VM every 2 s; no call may
    // leave a connection open against the VMM's small connection limit.
    #[tokio::test]
    async fn leaves_no_connection_open() {
        let ch = fake_ch(|_| ("200 OK", r#"{"state":"Running","memory_actual_size":2147483648}"#));
        for _ in 0..12 {
            Client::new(&ch.socket).info().await.unwrap();
        }
        for _ in 0..200 {
            if ch.open.load(Ordering::SeqCst) == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("{} API connections left open", ch.open.load(Ordering::SeqCst));
    }
}
