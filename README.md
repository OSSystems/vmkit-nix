# vmkit-nix

Building blocks for running VMs on [cloud-hypervisor] from NixOS hosts: a
Rust crate for controllers that boot and manage VMs, and a NixOS module for
what those controllers share when they run on one host. vmkit is not a
general cloud-hypervisor SDK; it is the part of a controller that does not
care which VMs it runs, throwaway or long-lived.

## The crate

```toml
[dependencies]
vmkit = { git = "https://github.com/OSSystems/vmkit-nix", tag = "v0.2.0" }
```

| Module | What it does |
|---|---|
| `ch` | The cloud-hypervisor command line from a typed `Vm` (kernel or firmware boot, disks, taps, vsock, serial, virtiofs shares, virtio-mem); the REST API `Client` (`info`, `resize_memory`, `power_button`); `dial_guest`, the host side of the vsock `CONNECT` handshake. |
| `vsock` | The guest side of vsock: `dial` the host, `listen` for it. |
| `control` | The guest-to-host control channel: one JSON request line, one reply line, and the `stats` request the memory policy reads. Callers add their own operations and reply fields. |
| `memory` | The elastic memory policy: guests boot small and grow or shrink 1 GiB at a time through virtio-mem by their memory pressure, keeping the host's reserve. `Status` is what one pool of VMs publishes so the other pools on the host account for it. |
| `virtiofs` | virtiofsd arguments, and `store_view`: a store holding exactly one Nix closure, so a guest sees its system and nothing else of the host's store. |
| `tap` | Persistent taps an unprivileged cloud-hypervisor can open. |
| `hostutil` | Accounts, systemd unit state, load, meminfo, atomic writes. |

vmkit never keeps a VM as its child: `ch::Vm::args` is a command line, and
the caller decides whether to exec it as a systemd unit's main process or
supervise it. I/O is async on tokio. Linux only.

What vmkit's processes exchange with each other and with guests (the pool
status files, the control channel) is a contract, described in
[docs/contracts.md](docs/contracts.md): controllers built on different vmkit
versions share a host as long as they keep to it. Version 0.1, in Go, keeps
to the same contract; it lives on the `go` branch.

## NixOS module

`nixosModules.default` declares `services.vmkit`, which controllers fill in
and which renders what they share on one host:

- `networks.<name>`: a guest network, its `/24` subnet and the interface
  guests come in on. Guests reach nothing on the host but replies, and no
  private range unless `forwardRules` allows it; they are NATed out of
  `externalInterface`. Subnets and chains must be unique.
- `memory.pools.<name>`: a pool of VMs with a `rank`. `first` pools grow
  before `yielding` ones, except into the `guaranteedGiB` a yielding pool
  may hold whatever first pools want. Each pool's controller publishes its
  `Status` at `statusFile` and reads the other rank's.
- `memory.reservePercent`, `memory.reserveMinGiB`: what every pool leaves to
  the host.

Controllers import the module themselves; it is declared once however many
import it. Make every controller's `vmkit` input follow the same one:

```nix
inputs = {
  vmkit.url = "github:OSSystems/vmkit-nix";
  controller-a.inputs.vmkit.follows = "vmkit";
  controller-b.inputs.vmkit.follows = "vmkit";
};
```

## Development

```sh
nix develop       # cargo, clippy, rustfmt, rust-analyzer
nix flake check   # rustfmt, clippy, the tests, and the module's evaluation tests
nix fmt -- $(git ls-files '*.nix')
```

[cloud-hypervisor]: https://github.com/cloud-hypervisor/cloud-hypervisor
