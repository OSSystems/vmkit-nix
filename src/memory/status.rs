use std::io;
use std::path::Path;

use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use super::policy::HOST_FULL_HOLD;
use crate::hostutil;

/// How old a published status may be before it counts as gone: its
/// controller stopped.
pub const STATUS_STALE: SignedDuration = SignedDuration::from_secs(30);

/// The most a status file may hold.
pub const STATUS_MAX_BYTES: u64 = 4096;

/// What one pool of VMs publishes about the memory it holds, so the other
/// pools on the host can account for it. A pool is whatever one controller
/// manages. Its file format is a contract: see `docs/contracts.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// When the controller wrote it.
    pub updated_at: Timestamp,
    /// What the pool's VMs were given plus room for each one's next plug.
    pub committed_bytes: i64,
    /// Memory promised to the pool's VMs but not yet taken.
    pub pending_bytes: i64,
    /// When one of the pool's VMs was last denied a plug.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denied_at: Option<Timestamp>,
}

/// Publishes `status` at `path`, creating its directory.
pub fn write_status(path: impl AsRef<Path>, status: &Status) -> io::Result<()> {
    let path = path.as_ref();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    hostutil::atomic_write(path, &serde_json::to_vec(status)?, 0o644)
}

/// Reads a published status; a missing, unreadable or stale one is `None`.
#[must_use]
pub fn read_status(path: impl AsRef<Path>, now: Timestamp) -> Option<Status> {
    let raw = hostutil::read_bounded(path, STATUS_MAX_BYTES).ok()?;
    let status: Status = serde_json::from_slice(&raw).ok()?;
    (now.duration_since(status.updated_at) <= STATUS_STALE).then_some(status)
}

/// Reads several pools' statuses as one: committed and pending memory add
/// up, the latest denial and the oldest update count. `None` when none of
/// them is published.
#[must_use]
pub fn read_statuses<P: AsRef<Path>>(paths: &[P], now: Timestamp) -> Option<Status> {
    paths.iter().filter_map(|p| read_status(p, now)).reduce(|sum, s| Status {
        updated_at: sum.updated_at.min(s.updated_at),
        committed_bytes: sum.committed_bytes + s.committed_bytes,
        pending_bytes: sum.pending_bytes + s.pending_bytes,
        denied_at: sum.denied_at.max(s.denied_at),
    })
}

// Pools sharing a host come in two ranks. A first pool, say long-lived VMs
// people work in, grows before a yielding pool, say throwaway VMs, except
// into the memory the yielding pool is guaranteed. Each pool publishes its
// Status and reads the other rank's.

/// What a yielding pool sees of the first pools on its host.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Ahead {
    /// The first pools' statuses, summed; `None` when none is published.
    pub status: Option<Status>,
    /// What the yielding pool may hold whatever the first pools want.
    pub guaranteed: i64,
}

impl Ahead {
    /// Whether a yielding pool holding `committed` gives way: past its
    /// guarantee it counts what the first pools hold as taken.
    #[must_use]
    pub fn yields(&self, committed: i64) -> bool {
        committed > self.guaranteed
    }

    /// Whether a first pool was denied memory recently; a yielding pool
    /// past its guarantee then neither grows nor boots.
    #[must_use]
    pub fn starved(&self, now: Timestamp) -> bool {
        self.status.and_then(|s| s.denied_at).is_some_and(|d| now.duration_since(d) < HOST_FULL_HOLD)
    }
}

/// What a first pool keeps free for a yielding pool: the part of its
/// guarantee it does not hold yet.
#[must_use]
pub fn owed(guaranteed: i64, behind: Option<&Status>) -> i64 {
    (guaranteed - behind.map_or(0, |s| s.committed_bytes)).max(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::GIB;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn noon() -> Timestamp {
        "2026-11-18T12:00:00Z".parse().unwrap()
    }

    fn s(secs: i64) -> SignedDuration {
        SignedDuration::from_secs(secs)
    }

    fn contract(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/contract").join(name)
    }

    #[test]
    fn round_trip_and_staleness() {
        let dir = tempfile::tempdir().unwrap();
        let now = noon();
        let path = dir.path().join("pools/a.json");
        let status =
            Status { updated_at: now, committed_bytes: 3 * GIB, pending_bytes: GIB, denied_at: Some(now - s(1)) };
        write_status(&path, &status).unwrap();
        assert_eq!(read_status(&path, now + s(1)), Some(status));
        assert_eq!(read_status(&path, now + STATUS_STALE + s(1)), None, "a stale status counted");
        assert_eq!(read_status(dir.path().join("missing.json"), now), None);
    }

    #[test]
    fn read_statuses_sums() {
        let dir = tempfile::tempdir().unwrap();
        let now = noon();
        let path = |n: &str| dir.path().join(n);
        write_status(
            path("a"),
            &Status { updated_at: now, committed_bytes: GIB, pending_bytes: GIB, denied_at: Some(now - s(5)) },
        )
        .unwrap();
        write_status(
            path("b"),
            &Status { updated_at: now - s(1), committed_bytes: 2 * GIB, pending_bytes: 0, denied_at: Some(now - s(1)) },
        )
        .unwrap();
        write_status(
            path("c"),
            &Status { updated_at: now - s(3600), committed_bytes: 100 * GIB, pending_bytes: 0, denied_at: None },
        )
        .unwrap();
        fs::write(path("garbage"), "{").unwrap();
        let sum = read_statuses(&[path("a"), path("b"), path("c"), path("garbage")], now).unwrap();
        assert_eq!(
            sum,
            Status {
                updated_at: now - s(1),
                committed_bytes: 3 * GIB,
                pending_bytes: GIB,
                denied_at: Some(now - s(1))
            }
        );
        assert_eq!(read_statuses(&[path("c")], now), None);
    }

    #[test]
    fn ranks() {
        let now = noon();
        let denied = Status { updated_at: now, committed_bytes: 0, pending_bytes: 0, denied_at: Some(now - s(10)) };
        let ahead = Ahead { status: Some(denied), guaranteed: 8 * GIB };
        assert!(!ahead.yields(8 * GIB) && ahead.yields(8 * GIB + 1), "yields only past the guarantee");
        assert!(
            ahead.starved(now) && !ahead.starved(now + HOST_FULL_HOLD),
            "starved only within HOST_FULL_HOLD of a denial"
        );
        assert!(!Ahead::default().starved(now));
        for (held, want) in [(0, 8 * GIB), (5 * GIB, 3 * GIB), (30 * GIB, 0)] {
            let behind = Status { committed_bytes: held, ..denied };
            assert_eq!(owed(8 * GIB, Some(&behind)), want, "holding {held}");
        }
        assert_eq!(owed(8 * GIB, None), 8 * GIB);
    }

    // docs/contracts.md: the pool status files every controller writes and
    // reads, from the fixtures the Go tests read too.
    #[test]
    fn contract_fixtures() {
        let updated: Timestamp = "2026-11-18T12:00:00.5Z".parse().unwrap();
        let want = Status {
            updated_at: updated,
            committed_bytes: 3 * GIB,
            pending_bytes: GIB,
            denied_at: Some("2026-11-18T11:59:59Z".parse().unwrap()),
        };
        for name in ["status.json", "status-offset.json"] {
            assert_eq!(read_status(contract(name), updated + s(1)), Some(want), "{name}");
        }
        let minimal = read_status(contract("status-minimal.json"), updated).unwrap();
        assert_eq!((minimal.denied_at, minimal.committed_bytes), (None, 0));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pool.json");
        for (status, fixture) in [
            (want, "status.json"),
            (
                Status {
                    updated_at: "2026-11-18T12:00:00Z".parse().unwrap(),
                    committed_bytes: 0,
                    pending_bytes: 0,
                    denied_at: None,
                },
                "status-minimal.json",
            ),
        ] {
            write_status(&path, &status).unwrap();
            assert_eq!(
                fs::read_to_string(&path).unwrap(),
                fs::read_to_string(contract(fixture)).unwrap().trim_end(),
                "{fixture}"
            );
        }
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o644);
    }

    #[test]
    fn contract_rejects() {
        let dir = tempfile::tempdir().unwrap();
        let now = noon();
        let big = format!(
            r#"{{"updatedAt":"2026-11-18T12:00:00Z","committedBytes":1,"pendingBytes":0,"pad":"{}"}}"#,
            "x".repeat(4096)
        );
        for (name, content) in [
            ("stale", r#"{"updatedAt":"2026-11-18T11:59:29Z","committedBytes":1,"pendingBytes":0}"#.to_owned()),
            ("garbage", r#"{"updatedAt":"#.to_owned()),
            ("big", big),
        ] {
            let path = dir.path().join(name);
            fs::write(&path, content).unwrap();
            assert_eq!(read_status(&path, now), None, "{name} counted");
        }
    }
}
