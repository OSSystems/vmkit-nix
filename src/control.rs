//! The guest-to-host control channel: one JSON request line a guest sends
//! to a vsock port, one JSON reply line back. vmkit defines the framing and
//! the `stats` request its memory policy needs; callers add their own
//! operations and reply fields. The wire format is a contract: see
//! `docs/contracts.md`.
//!
//! The socket a request comes in on identifies the VM: the host side of a
//! guest's vsock port P is the VM's own `<vsock socket>_P`, so nothing in
//! the payload is trusted to name it.

use std::future::Future;
use std::io;
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::time::Instant;

use crate::memory::GuestStats;

/// The most of a request the host reads.
pub const MAX_REQUEST: u64 = 4096;
/// How long a guest has to send its request and take the reply.
pub const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(5);

/// One request. Callers with their own operations parse the line
/// themselves, with [`Exchange::read_line`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    /// The operation; empty when the request has none, which callers
    /// answer as an unknown operation.
    #[serde(default)]
    pub op: String,
    /// `stats`: the guest's `MemAvailable`.
    #[serde(rename = "memAvailableKiB", skip_serializing_if = "Option::is_none")]
    pub mem_available_kib: Option<i64>,
    /// `stats`: the guest's `MemTotal`.
    #[serde(rename = "memTotalKiB", skip_serializing_if = "Option::is_none")]
    pub mem_total_kib: Option<i64>,
    /// `stats`: PSI `some avg10` for memory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub psi_some_avg10: Option<f64>,
    /// `stats`: PSI `full avg10` for memory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub psi_full_avg10: Option<f64>,
}

/// The part of a reply every operation has. Callers add fields with a
/// struct that holds it under `#[serde(flatten)]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    /// The operation succeeded.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ok: bool,
    /// Why it failed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
}

impl Reply {
    /// A success.
    #[must_use]
    pub fn ok() -> Self {
        Reply { ok: true, error: String::new() }
    }

    /// A failure.
    pub fn error(reason: impl Into<String>) -> Self {
        Reply { ok: false, error: reason.into() }
    }
}

impl Request {
    /// Parses a request line; a failure comes back as the reply to send.
    pub fn parse(line: &[u8]) -> Result<Request, Reply> {
        serde_json::from_slice(line).map_err(|_| Reply::error("invalid JSON"))
    }

    /// The report a `stats` request carries, received at `now`.
    pub fn stats(&self, now: Timestamp) -> Result<GuestStats, Reply> {
        let (Some(available), Some(total), Some(some), Some(full)) =
            (self.mem_available_kib, self.mem_total_kib, self.psi_some_avg10, self.psi_full_avg10)
        else {
            return Err(Reply::error("incomplete stats"));
        };
        let s = GuestStats {
            mem_available_kib: available,
            mem_total_kib: total,
            psi_some_avg10: some,
            psi_full_avg10: full,
            received_at: now,
        };
        if s.valid() { Ok(s) } else { Err(Reply::error("invalid stats")) }
    }

    /// The `stats` request a guest sends, read from the text of its
    /// `/proc/meminfo` and `/proc/pressure/memory`. A guest without PSI
    /// reports no pressure.
    pub fn stats_from(meminfo: &str, pressure: &str) -> Result<Request, NoMeminfo> {
        let mem = crate::hostutil::parse_meminfo(meminfo);
        let (Some(&available), Some(&total)) = (mem.get("MemAvailable"), mem.get("MemTotal")) else {
            return Err(NoMeminfo);
        };
        let (mut some, mut full) = (0.0, 0.0);
        for line in pressure.lines() {
            let mut fields = line.split_whitespace();
            let (Some(kind), Some(avg10)) = (fields.next(), fields.next()) else { continue };
            let Some(Ok(v)) = avg10.strip_prefix("avg10=").map(str::parse::<f64>) else { continue };
            match kind {
                "some" => some = v,
                "full" => full = v,
                _ => {}
            }
        }
        Ok(Request {
            op: "stats".into(),
            mem_available_kib: Some(available),
            mem_total_kib: Some(total),
            psi_some_avg10: Some(some),
            psi_full_avg10: Some(full),
        })
    }
}

/// `/proc/meminfo` has no `MemAvailable` or `MemTotal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("no MemAvailable or MemTotal in /proc/meminfo")]
pub struct NoMeminfo;

/// The host side of one request and its reply, within
/// [`EXCHANGE_TIMEOUT`] of the connection.
pub struct Exchange<S> {
    stream: BufReader<S>,
    deadline: Instant,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Exchange<S> {
    /// Starts the exchange on a connection a guest opened.
    pub fn new(stream: S) -> Self {
        Exchange { stream: BufReader::new(stream), deadline: Instant::now() + EXCHANGE_TIMEOUT }
    }

    /// Reads the request line, without its `\n`, of at most
    /// [`MAX_REQUEST`] bytes; a failure comes back as the reply to send.
    pub async fn read_line(&mut self) -> Result<Vec<u8>, Reply> {
        let mut line = Vec::new();
        let mut limited = (&mut self.stream).take(MAX_REQUEST);
        let read = limited.read_until(b'\n', &mut line);
        match tokio::time::timeout_at(self.deadline, read).await {
            Ok(Ok(_)) if !line.is_empty() => {
                if line.last() == Some(&b'\n') {
                    line.pop();
                }
                Ok(line)
            }
            _ => Err(Reply::error("expected one JSON line")),
        }
    }

    /// Reads a [`Request`].
    pub async fn read_request(&mut self) -> Result<Request, Reply> {
        Request::parse(&self.read_line().await?)
    }

    /// Sends the reply and closes the connection. Returns whether the reply
    /// was delivered, for callers whose reply hands something out once.
    pub async fn reply(mut self, reply: &impl Serialize) -> bool {
        let Ok(mut line) = serde_json::to_vec(reply) else { return false };
        line.push(b'\n');
        let stream = self.stream.get_mut();
        let send = async {
            stream.write_all(&line).await?;
            stream.shutdown().await
        };
        matches!(tokio::time::timeout_at(self.deadline, send).await, Ok(Ok(())))
    }
}

/// Hands every connection on `listener` to `handle`, each in its own task,
/// until accepting fails for good.
pub async fn serve<F, Fut>(listener: UnixListener, handle: F) -> io::Error
where
    F: Fn(UnixStream) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                tokio::spawn(handle(stream));
            }
            Err(e) => match e.raw_os_error() {
                Some(libc::ECONNABORTED | libc::EINTR | libc::EPROTO) => {}
                // Out of descriptors or memory: wait for some to free up.
                Some(libc::EMFILE | libc::ENFILE | libc::ENOBUFS | libc::ENOMEM) => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                _ => return e,
            },
        }
    }
}

/// Reads one `\n`- or `\r\n`-terminated line, without it, of at most
/// `limit` bytes; a last line without `\n` counts.
pub async fn read_line(r: &mut (impl AsyncBufRead + Unpin), limit: u64) -> io::Result<Vec<u8>> {
    let mut line = Vec::new();
    r.take(limit + 1).read_until(b'\n', &mut line).await?;
    if line.last() == Some(&b'\n') {
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
    } else if line.is_empty() {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    if line.len() as u64 > limit {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("line longer than {limit} bytes")));
    }
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    fn contract(name: &str) -> String {
        std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/contract").join(name))
            .unwrap()
    }

    #[test]
    fn stats_from_proc() {
        let meminfo = "MemTotal:        2014532 kB\nMemFree:          100000 kB\nMemAvailable:     812345 kB\n";
        let pressure =
            "some avg10=7.25 avg60=1.00 avg300=0.10 total=123\nfull avg10=0.50 avg60=0.00 avg300=0.00 total=4\n";
        let req = Request::stats_from(meminfo, pressure).unwrap();
        assert_eq!(
            (req.op.as_str(), req.mem_total_kib, req.mem_available_kib, req.psi_some_avg10, req.psi_full_avg10),
            ("stats", Some(2_014_532), Some(812_345), Some(7.25), Some(0.5))
        );
        assert_eq!(Request::stats_from(meminfo, "").unwrap().psi_some_avg10, Some(0.0), "without PSI");
        assert_eq!(Request::stats_from("MemFree: 1 kB\n", pressure), Err(NoMeminfo));
    }

    #[test]
    fn request_stats() {
        let now = Timestamp::now();
        assert_eq!(Request::parse(b"not json"), Err(Reply::error("invalid JSON")));
        assert_eq!(Request::parse(b"{}").unwrap().op, "", "a request without op parses");
        let req = Request::parse(br#"{"op":"stats","memAvailableKiB":5}"#).unwrap();
        assert_eq!(req.stats(now), Err(Reply::error("incomplete stats")));
        let req =
            Request::parse(br#"{"op":"stats","memAvailableKiB":5,"memTotalKiB":4,"psiSomeAvg10":0,"psiFullAvg10":0}"#)
                .unwrap();
        assert_eq!(req.stats(now), Err(Reply::error("invalid stats")), "available above total");
        let req = Request::parse(
            br#"{"op":"stats","memAvailableKiB":524288,"memTotalKiB":2097152,"psiSomeAvg10":12,"psiFullAvg10":1}"#,
        )
        .unwrap();
        let s = req.stats(now).unwrap();
        assert!(s.pressured());
        assert_eq!(s.received_at, now);
    }

    // A caller's reply holds Reply flattened and adds its own fields.
    #[tokio::test]
    async fn exchange_writes_caller_replies() {
        #[derive(Serialize)]
        struct JitReply {
            #[serde(flatten)]
            reply: Reply,
            #[serde(skip_serializing_if = "String::is_empty")]
            jitconfig: String,
        }
        let (host, mut guest) = duplex(8192);
        let server = tokio::spawn(async move {
            let mut exchange = Exchange::new(host);
            let req = exchange.read_request().await.unwrap();
            exchange.reply(&JitReply { reply: Reply::default(), jitconfig: format!("cfg-{}", req.op) }).await
        });
        guest.write_all(b"{\"op\":\"jit\"}\n").await.unwrap();
        let line = read_line(&mut BufReader::new(&mut guest), 4096).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&line).unwrap(),
            serde_json::json!({"jitconfig": "cfg-jit"})
        );
        assert!(server.await.unwrap(), "delivery not reported");
    }

    #[tokio::test]
    async fn exchange_rejects_bad_framing() {
        // Nothing at all.
        let (host, guest) = duplex(8192);
        drop(guest);
        assert_eq!(Exchange::new(host).read_line().await, Err(Reply::error("expected one JSON line")));

        // A last line without its newline still counts.
        let (host, mut guest) = duplex(8192);
        guest.write_all(b"{\"op\":\"x\"}").await.unwrap();
        drop(guest);
        assert_eq!(Exchange::new(host).read_request().await.unwrap().op, "x");

        // More than MAX_REQUEST bytes is cut there and is not JSON.
        let (host, mut guest) = duplex(16384);
        let long = format!("{{\"op\":\"{}\"}}\n", "x".repeat(5000));
        guest.write_all(long.as_bytes()).await.unwrap();
        assert_eq!(Exchange::new(host).read_request().await, Err(Reply::error("invalid JSON")));
    }

    #[tokio::test(start_paused = true)]
    async fn exchange_times_out() {
        let (host, _guest) = duplex(8192);
        assert_eq!(Exchange::new(host).read_line().await, Err(Reply::error("expected one JSON line")));
    }

    #[tokio::test]
    async fn reply_to_a_gone_guest_is_not_delivered() {
        let (host, guest) = duplex(8);
        drop(guest);
        assert!(!Exchange::new(host).reply(&Reply::ok()).await);
    }

    #[tokio::test]
    async fn read_line_limit() {
        let long = format!("{}\n", "x".repeat(100));
        let mut r = BufReader::with_capacity(16, long.as_bytes());
        assert!(read_line(&mut r, 50).await.is_err(), "over-long line accepted");
        let mut r = BufReader::new(&b"exactly\n"[..]);
        assert_eq!(read_line(&mut r, 7).await.unwrap(), b"exactly");
        let mut r = BufReader::new(&b"crlf\r\nnext"[..]);
        assert_eq!(read_line(&mut r, 7).await.unwrap(), b"crlf");
        assert_eq!(read_line(&mut r, 7).await.unwrap(), b"next");
        let mut r = BufReader::new(&b""[..]);
        assert_eq!(read_line(&mut r, 7).await.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[tokio::test]
    async fn serves_connections() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("control.sock");
        let listener = UnixListener::bind(&path).unwrap();
        tokio::spawn(serve(listener, |stream| async move {
            let mut exchange = Exchange::new(stream);
            let reply = match exchange.read_request().await {
                Ok(req) if req.op == "stats" => Reply::ok(),
                Ok(_) => Reply::error("unknown op"),
                Err(bad) => bad,
            };
            exchange.reply(&reply).await;
        }));
        for (request, want) in
            [("{\"op\":\"stats\"}\n", "{\"ok\":true}"), ("{\"op\":\"nope\"}\n", "{\"error\":\"unknown op\"}")]
        {
            let mut conn = UnixStream::connect(&path).await.unwrap();
            conn.write_all(request.as_bytes()).await.unwrap();
            let line = read_line(&mut BufReader::new(conn), 4096).await.unwrap();
            assert_eq!(String::from_utf8(line).unwrap(), want);
        }
    }

    // docs/contracts.md: the control channel's framing and the stats
    // request, from the fixtures the Go tests on the
    // go branch read too.
    #[test]
    fn contract_fixtures() {
        let req = Request::stats_from(&contract("meminfo.txt"), &contract("pressure.txt")).unwrap();
        assert_eq!(serde_json::to_string(&req).unwrap(), contract("stats-request.json").trim_end());
        let parsed = Request::parse(contract("stats-request.json").trim_end().as_bytes()).unwrap();
        let s = parsed.stats(Timestamp::now()).unwrap();
        assert_eq!(
            (s.mem_available_kib, s.mem_total_kib, s.psi_some_avg10, s.psi_full_avg10),
            (812_345, 8_388_608, 7.25, 0.5)
        );
        // Integral PSI averages are numbers too.
        let parsed = Request::parse(
            br#"{"op":"stats","memAvailableKiB":1,"memTotalKiB":2,"psiSomeAvg10":0.0,"psiFullAvg10":3}"#,
        )
        .unwrap();
        assert!(parsed.stats(Timestamp::now()).is_ok());
        for (fixture, reply) in [("reply-ok.json", Reply::ok()), ("reply-error.json", Reply::error("invalid stats"))] {
            assert_eq!(serde_json::to_string(&reply).unwrap(), contract(fixture).trim_end(), "{fixture}");
        }
    }
}
