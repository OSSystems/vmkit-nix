# Contracts

What vmkit's processes exchange with each other and with guests. Controllers
built on different vmkit versions, or in different languages, share one host
as long as they keep to these; a change here is a breaking change. The
fixtures in [`testdata/contract`](../testdata/contract) are the examples every
implementation's tests read.

## Pool status files

Every memory pool's controller publishes what its VMs hold at
`/run/vmkit/memory/<pool>.json` (`services.vmkit.memory.pools.<pool>.statusFile`),
about every 2 s, and reads the other rank's.

```json
{"updatedAt":"2026-11-18T12:00:00.5Z","committedBytes":3221225472,"pendingBytes":1073741824,"deniedAt":"2026-11-18T11:59:59Z"}
```

| Field | Type | Meaning |
|---|---|---|
| `updatedAt` | RFC 3339 time | When the controller wrote it. |
| `committedBytes` | integer | What the pool's VMs were given plus room for each one's next plug: memory they may touch at any moment. |
| `pendingBytes` | integer | Memory promised but not yet taken: VMs that have not reported yet, and plugs their guest does not show yet. |
| `deniedAt` | RFC 3339 time, optional | When one of the pool's VMs was last denied a plug. Absent if never. |

- Writers replace the file atomically (a temporary file in the same
  directory, renamed over it) with mode 0644, and keep it under 4096 bytes.
- Times may carry any UTC offset and up to nanoseconds; writers may use
  either `Z` or an offset.
- Readers ignore unknown fields. A file that is missing, unreadable, larger
  than 4096 bytes, not this JSON, or whose `updatedAt` is more than 30 s old
  counts as absent: its controller stopped.
- A `first` pool reads the `yielding` pools' files to keep what they are
  owed; a `yielding` pool reads the `first` pools' files, summed: committed
  and pending add up, `deniedAt` is the latest, `updatedAt` the oldest.

Fixtures: `status.json`, `status-offset.json` (the same status, written with
a local offset and nanoseconds), `status-minimal.json` (no `deniedAt`).

## Control channel

A guest reaches its host on a vsock port the controller picks. Cloud-hypervisor
forwards the connection to `<vsock socket>_<port>` on the host, which only
that VM can reach, so the socket a request arrives on names the VM and
nothing in the payload is trusted to.

- The guest sends one request: a JSON object on one line, ended by `\n`.
  The host reads at most 4096 bytes and waits at most 5 s.
- The host answers one JSON object on one line, ended by `\n`, and closes
  the connection.
- Every request has `op`. Every reply has `ok: true` or `error: "<reason>"`,
  never both, plus whatever fields the operation adds.
- Failures vmkit defines: `expected one JSON line`, `invalid JSON`,
  `incomplete stats`, `invalid stats`. A controller answers an operation it
  does not know with `unknown op`.

### `stats`

The guest's memory report, every 2 s, from its `/proc/meminfo` and
`/proc/pressure/memory`:

```json
{"op":"stats","memAvailableKiB":812345,"memTotalKiB":8388608,"psiSomeAvg10":7.25,"psiFullAvg10":0.5}
```

All four fields are required. `memAvailableKiB` and `memTotalKiB` are
integers, `0 ≤ memAvailableKiB ≤ memTotalKiB` and `memTotalKiB > 0`; the PSI
averages are numbers from 0 to 100, either `0` or `0.0`. A guest without PSI
reports 0. The reply is `{"ok":true}`.

Fixtures: `meminfo.txt` and `pressure.txt`, and the request they make,
`stats-request.json`; `reply-ok.json`, `reply-error.json`.

## Host to guest over vsock

The host reaches a guest's vsock port through cloud-hypervisor's socket:
it writes `CONNECT <port>\n` and reads `OK <host port>\n` before the guest's
first byte. This is cloud-hypervisor's protocol, not vmkit's; what flows
after it is up to the controller.

## The NixOS module

`services.vmkit` is the third contract: every controller on a host imports
the same module (their `vmkit` inputs follow one), and the options in the
[README](../README.md#nixos-module) keep their names and meaning.
