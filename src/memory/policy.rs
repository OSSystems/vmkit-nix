use std::fmt;

use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

/// A mebibyte.
pub const MIB: i64 = 1 << 20;
/// A gibibyte.
pub const GIB: i64 = 1 << 30;
/// How much memory one plug or unplug moves.
pub const MEMORY_STEP: i64 = GIB;
/// How old guest stats may be before memory stops changing.
pub const STATS_STALE: SignedDuration = SignedDuration::from_secs(10);
/// How long a guest must have had spare memory and no pressure before
/// memory is taken back.
pub const UNPLUG_CALM: SignedDuration = SignedDuration::from_secs(30);
/// How long a plug may take to show in the guest's `MemTotal` before it
/// stops counting as pending.
pub const PLUG_SETTLE: SignedDuration = SignedDuration::from_secs(60);
/// Absorbs memory the guest kernel keeps for itself out of a plug.
pub const PLUG_SLACK: i64 = 128 * MIB;
/// How long admission stays closed after a plug was denied: running guests
/// come first.
pub const HOST_FULL_HOLD: SignedDuration = SignedDuration::from_secs(30);

/// One report a guest sent on the control channel.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuestStats {
    /// The guest's `MemAvailable`.
    #[serde(rename = "memAvailableKiB")]
    pub mem_available_kib: i64,
    /// The guest's `MemTotal`: boot memory plus what it has onlined.
    #[serde(rename = "memTotalKiB")]
    pub mem_total_kib: i64,
    /// The share of the last 10 s some task stalled on memory, in percent.
    pub psi_some_avg10: f64,
    /// The share of the last 10 s every task stalled on memory, in percent.
    pub psi_full_avg10: f64,
    /// When the host received the report.
    pub received_at: Timestamp,
}

impl GuestStats {
    /// Whether the report is one a guest could send.
    #[must_use]
    pub fn valid(&self) -> bool {
        let percent = |v: f64| v.is_finite() && (0.0..=100.0).contains(&v);
        self.mem_available_kib >= 0
            && self.mem_total_kib > 0
            && self.mem_available_kib <= self.mem_total_kib
            && percent(self.psi_some_avg10)
            && percent(self.psi_full_avg10)
    }

    /// `MemAvailable` in bytes.
    #[must_use]
    pub fn available(&self) -> i64 {
        self.mem_available_kib.saturating_mul(1024)
    }

    /// Whether the guest needs more memory.
    #[must_use]
    pub fn pressured(&self) -> bool {
        self.psi_some_avg10 > 5.0 || self.available() < GIB
    }

    /// Whether the guest could give memory back.
    #[must_use]
    pub fn calm(&self) -> bool {
        self.psi_some_avg10 < 0.1 && self.available() > 3 * GIB
    }
}

/// The host side of a memory decision, in bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HostMemory {
    /// The host's `MemAvailable`.
    pub available: i64,
    /// What the host keeps for itself, plus what it keeps for other pools.
    pub reserve: i64,
    /// Memory promised but not yet taken: VMs that have not reported yet
    /// and plugs their guest has not reported since.
    pub pending: i64,
}

impl HostMemory {
    /// What may still be handed out without touching the reserve.
    #[must_use]
    pub fn spare(&self) -> i64 {
        self.available - self.reserve - self.pending
    }
}

/// A memory decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Change nothing.
    Hold,
    /// Plug one step.
    Plug,
    /// Unplug one step.
    Unplug,
    /// The guest needs a step the host cannot spare.
    Denied,
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Action::Hold => "hold",
            Action::Plug => "plug",
            Action::Unplug => "unplug",
            Action::Denied => "denied",
        })
    }
}

/// What the policy knows about one VM.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VmMemory {
    /// The memory the VM boots with.
    pub boot_bytes: i64,
    /// The most it may grow to.
    pub max_bytes: i64,
    /// Boot memory plus what virtio-mem has plugged; an unplug may leave it
    /// above the requested size.
    pub actual_bytes: i64,
    /// The guest's latest report, if it sent one.
    pub stats: Option<GuestStats>,
    /// The last resize request; stats older than it do not show its effect
    /// yet.
    pub resized_at: Option<Timestamp>,
    /// When the guest last became calm, if it is.
    pub calm_since: Option<Timestamp>,
    /// The last plug while the guest's `MemTotal` does not show it yet.
    pub pending_plug: i64,
    /// The `MemTotal` the pending plug was granted on.
    pub plug_base_kib: i64,
}

impl VmMemory {
    /// A new VM's memory: the boot size, growing up to `max_bytes`.
    #[must_use]
    pub fn new(boot_bytes: i64, max_bytes: i64) -> Self {
        VmMemory {
            boot_bytes,
            max_bytes,
            actual_bytes: boot_bytes,
            stats: None,
            resized_at: None,
            calm_since: None,
            pending_plug: 0,
            plug_base_kib: 0,
        }
    }

    /// Takes a guest report into account: calm tracking, and the end of a
    /// pending plug once `MemTotal` shows it. A report older than the last
    /// one is ignored.
    pub fn observe(&mut self, s: GuestStats) {
        if self.stats.is_some_and(|last| last.received_at > s.received_at) {
            return;
        }
        self.stats = Some(s);
        if s.calm() {
            self.calm_since.get_or_insert(s.received_at);
        } else {
            self.calm_since = None;
        }
        if self.pending_plug > 0
            && let Some(resized) = self.resized_at
            && s.received_at > resized
        {
            let shown = s.mem_total_kib.saturating_mul(1024)
                >= self.plug_base_kib.saturating_mul(1024) + self.pending_plug - PLUG_SLACK;
            if shown || s.received_at.duration_since(resized) > PLUG_SETTLE {
                self.pending_plug = 0;
            }
        }
    }

    /// Records a resize from `actual` to `target` before it is requested,
    /// so a plug counts as pending from the moment it is decided. Returns
    /// the state to restore if the request fails.
    #[allow(clippy::return_self_not_must_use)]
    pub fn grant(&mut self, actual: i64, target: i64, at: Timestamp) -> VmMemory {
        let previous = *self;
        self.resized_at = Some(at);
        self.pending_plug = 0;
        if target > actual {
            self.pending_plug = target - actual;
            self.plug_base_kib = self.stats.map_or(0, |s| s.mem_total_kib);
        } else {
            // One unplug per calm period: the guest must stay calm for
            // another UNPLUG_CALM before the next step.
            self.calm_since = None;
        }
        previous
    }

    /// Memory this VM was promised but has not been seen to take: its boot
    /// memory until it first reports, then a plug its guest does not show
    /// yet.
    #[must_use]
    pub fn pending(&self) -> i64 {
        if self.stats.is_none() { self.boot_bytes } else { self.pending_plug }
    }

    /// What this VM was given, boot plus plugged plus a plug in flight, and
    /// room for its next plug: memory it may touch at any moment.
    #[must_use]
    pub fn committed(&self) -> i64 {
        self.actual_bytes + self.pending_plug + MEMORY_STEP
    }
}

/// Decides one VM's next memory change and the size it leads to: plug one
/// step under guest pressure if the host keeps its reserve, unplug one step
/// after [`UNPLUG_CALM`] of calm, never below boot memory, and nothing on
/// stale stats, before the guest reported after the last resize, or while
/// the last plug is not visible yet.
#[must_use]
pub fn decide(vm: &VmMemory, host: HostMemory, now: Timestamp) -> (Action, i64) {
    let hold = (Action::Hold, vm.actual_bytes);
    let Some(s) = vm.stats else { return hold };
    let age = now.duration_since(s.received_at);
    if age > STATS_STALE || age < -SignedDuration::from_secs(1) || !s.valid() {
        return hold;
    }
    if vm.resized_at.is_some_and(|r| s.received_at <= r) || vm.pending_plug > 0 {
        return hold;
    }
    if s.pressured() {
        let target = vm.max_bytes.min(vm.actual_bytes + MEMORY_STEP);
        if target <= vm.actual_bytes {
            return hold;
        }
        if host.spare() < target - vm.actual_bytes {
            return (Action::Denied, vm.actual_bytes);
        }
        return (Action::Plug, target);
    }
    if s.calm() && vm.calm_since.is_some_and(|c| now.duration_since(c) >= UNPLUG_CALM) {
        let target = vm.boot_bytes.max(vm.actual_bytes - MEMORY_STEP);
        if target < vm.actual_bytes {
            return (Action::Unplug, target);
        }
    }
    hold
}

/// What the host keeps for itself: the larger of `percent` of its memory
/// and `min_gib`.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
pub fn reserve(total_bytes: i64, percent: f64, min_gib: f64) -> i64 {
    ((total_bytes as f64 * percent / 100.0) as i64).max((min_gib * GIB as f64) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> Timestamp {
        Timestamp::from_second(1_800_000_000 + secs).unwrap()
    }

    fn s(secs: i64) -> SignedDuration {
        SignedDuration::from_secs(secs)
    }

    type Case = (&'static str, Box<dyn Fn(&mut VmMemory)>, HostMemory, Action, i64);

    #[test]
    #[allow(clippy::too_many_lines)]
    fn decision() {
        let now = at(0);
        let stats = move |avail_gib: i64, psi: f64| GuestStats {
            mem_available_kib: avail_gib << 20,
            mem_total_kib: 8 << 20,
            psi_some_avg10: psi,
            psi_full_avg10: 0.0,
            received_at: now - s(1),
        };
        let base = VmMemory { actual_bytes: 4 * GIB, ..VmMemory::new(2 * GIB, 10 * GIB) };
        let roomy = HostMemory { available: 40 * GIB, reserve: 6 * GIB, pending: 0 };
        let cases: Vec<Case> = vec![
            ("psi plugs a step", Box::new(move |v| v.stats = Some(stats(2, 6.0))), roomy, Action::Plug, 5 * GIB),
            ("low available plugs", Box::new(move |v| v.stats = Some(stats(0, 0.0))), roomy, Action::Plug, 5 * GIB),
            (
                "plug capped at hotplug size",
                Box::new(move |v| {
                    v.stats = Some(stats(0, 50.0));
                    v.actual_bytes = 10 * GIB;
                }),
                roomy,
                Action::Hold,
                10 * GIB,
            ),
            (
                "host reserve denies",
                Box::new(move |v| v.stats = Some(stats(0, 50.0))),
                HostMemory { available: 7 * GIB, reserve: 6 * GIB, pending: 1 },
                Action::Denied,
                4 * GIB,
            ),
            (
                "pending memory denies",
                Box::new(move |v| v.stats = Some(stats(0, 50.0))),
                HostMemory { available: 10 * GIB, reserve: 6 * GIB, pending: 4 * GIB },
                Action::Denied,
                4 * GIB,
            ),
            (
                "plug that exactly fits",
                Box::new(move |v| v.stats = Some(stats(0, 50.0))),
                HostMemory { available: 7 * GIB, reserve: 6 * GIB, pending: 0 },
                Action::Plug,
                5 * GIB,
            ),
            (
                "stale stats freeze",
                Box::new(move |v| v.stats = Some(GuestStats { received_at: now - s(11), ..stats(0, 50.0) })),
                roomy,
                Action::Hold,
                4 * GIB,
            ),
            (
                "stats from the future freeze",
                Box::new(move |v| v.stats = Some(GuestStats { received_at: now + s(2), ..stats(0, 50.0) })),
                roomy,
                Action::Hold,
                4 * GIB,
            ),
            (
                "invalid stats freeze",
                Box::new(move |v| v.stats = Some(GuestStats { psi_some_avg10: f64::NAN, ..stats(0, 50.0) })),
                roomy,
                Action::Hold,
                4 * GIB,
            ),
            ("no stats yet", Box::new(|_| {}), roomy, Action::Hold, 4 * GIB),
            (
                "waits for stats after resize",
                Box::new(move |v| {
                    v.stats = Some(stats(0, 50.0));
                    v.resized_at = Some(now);
                }),
                roomy,
                Action::Hold,
                4 * GIB,
            ),
            (
                "stats after resize count",
                Box::new(move |v| {
                    v.stats = Some(stats(0, 50.0));
                    v.resized_at = Some(now - s(2));
                }),
                roomy,
                Action::Plug,
                5 * GIB,
            ),
            (
                "calm long enough unplugs",
                Box::new(move |v| {
                    v.stats = Some(stats(4, 0.0));
                    v.calm_since = Some(now - s(31));
                }),
                roomy,
                Action::Unplug,
                3 * GIB,
            ),
            (
                "calm too short holds",
                Box::new(move |v| {
                    v.stats = Some(stats(4, 0.0));
                    v.calm_since = Some(now - s(20));
                }),
                roomy,
                Action::Hold,
                4 * GIB,
            ),
            (
                "spare but some pressure holds",
                Box::new(move |v| {
                    v.stats = Some(stats(4, 1.0));
                    v.calm_since = Some(now - s(60));
                }),
                roomy,
                Action::Hold,
                4 * GIB,
            ),
            (
                "never below boot",
                Box::new(move |v| {
                    v.stats = Some(stats(4, 0.0));
                    v.actual_bytes = 2 * GIB;
                    v.calm_since = Some(now - s(60));
                }),
                roomy,
                Action::Hold,
                2 * GIB,
            ),
            (
                "partial unplug floors at boot",
                Box::new(move |v| {
                    v.stats = Some(stats(4, 0.0));
                    v.actual_bytes = 2 * GIB + 512 * MIB;
                    v.calm_since = Some(now - s(60));
                }),
                roomy,
                Action::Unplug,
                2 * GIB,
            ),
        ];
        for (name, setup, host, action, target) in cases {
            let mut vm = base;
            setup(&mut vm);
            assert_eq!(decide(&vm, host, now), (action, target), "{name}");
        }
    }

    #[test]
    fn plug_pending_until_guest_shows_it() {
        let pressured = |t: Timestamp, total_mib: i64| GuestStats {
            mem_available_kib: 256 << 10,
            mem_total_kib: total_mib << 10,
            psi_some_avg10: 30.0,
            psi_full_avg10: 0.0,
            received_at: t,
        };
        let roomy = HostMemory { available: 40 * GIB, reserve: 6 * GIB, pending: 0 };
        let mut m = VmMemory { actual_bytes: 4 * GIB, ..VmMemory::new(2 * GIB, 10 * GIB) };
        m.observe(pressured(at(0), 3900));
        m.grant(4 * GIB, 5 * GIB, at(1));
        assert_eq!(m.pending(), GIB);
        // Reported after the resize, but virtio-mem has not onlined the
        // block yet.
        m.observe(pressured(at(3), 3900));
        assert_eq!(m.pending_plug, GIB, "the plug is still pending");
        assert_eq!(decide(&m, roomy, at(4)).0, Action::Hold, "plugged again before the last plug showed");
        m.observe(pressured(at(5), 3900 + 1024 - 100));
        assert_eq!(m.pending_plug, 0, "plug still pending after MemTotal grew by it");
        assert_eq!(decide(&m, roomy, at(6)), (Action::Plug, 5 * GIB));
        // A plug that never shows stops counting after PLUG_SETTLE.
        m.grant(5 * GIB, 6 * GIB, at(7));
        m.observe(pressured(at(8) + PLUG_SETTLE, 3900));
        assert_eq!(m.pending_plug, 0, "plug pending forever");
    }

    // After an unplug the guest must stay calm for another UNPLUG_CALM: one
    // step per period, not one per report.
    #[test]
    fn unplug_once_per_calm_period() {
        let calm = |t: Timestamp| GuestStats {
            mem_available_kib: 6 << 20,
            mem_total_kib: 8 << 20,
            psi_some_avg10: 0.0,
            psi_full_avg10: 0.0,
            received_at: t,
        };
        let roomy = HostMemory { available: 40 * GIB, reserve: 6 * GIB, pending: 0 };
        let mut m = VmMemory { actual_bytes: 8 * GIB, ..VmMemory::new(2 * GIB, 10 * GIB) };
        m.observe(calm(at(0)));
        m.observe(calm(at(31)));
        let (action, target) = decide(&m, roomy, at(31));
        assert_eq!(action, Action::Unplug);
        m.grant(m.actual_bytes, target, at(31));
        m.actual_bytes = target;
        m.observe(calm(at(33)));
        assert_eq!(decide(&m, roomy, at(33)).0, Action::Hold, "unplugged again 2 s after an unplug");
        m.observe(calm(at(64)));
        assert_eq!(decide(&m, roomy, at(64)).0, Action::Unplug, "no unplug after another calm period");
    }

    #[test]
    fn observe_ignores_older_reports() {
        let report = |t: Timestamp, avail_gib: i64| GuestStats {
            mem_available_kib: avail_gib << 20,
            mem_total_kib: 8 << 20,
            psi_some_avg10: 0.0,
            psi_full_avg10: 0.0,
            received_at: t,
        };
        let mut m = VmMemory::new(2 * GIB, 10 * GIB);
        assert_eq!(m.pending(), 2 * GIB, "boot memory is pending until the first report");
        m.observe(report(at(10), 6));
        m.observe(report(at(5), 0));
        assert_eq!(m.stats.unwrap().received_at, at(10));
        assert_eq!(m.calm_since, Some(at(10)));
        assert_eq!(m.pending(), 0);
        assert_eq!(m.committed(), 2 * GIB + MEMORY_STEP);
    }

    #[test]
    fn failed_grant_restores() {
        let mut m = VmMemory { calm_since: Some(at(0)), ..VmMemory::new(2 * GIB, 10 * GIB) };
        let before = m;
        let previous = m.grant(2 * GIB, GIB, at(40));
        assert_eq!(m.calm_since, None);
        assert_eq!(previous, before);
    }

    #[test]
    fn reserve_takes_the_larger() {
        assert_eq!(reserve(64 * GIB, 10.0, 2.0), 64 * GIB / 10);
        assert_eq!(reserve(8 * GIB, 10.0, 2.0), 2 * GIB);
        assert_eq!(Action::Denied.to_string(), "denied");
    }
}
