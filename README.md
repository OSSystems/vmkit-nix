# vmkit-nix

Building blocks for running VMs on [cloud-hypervisor] from NixOS hosts: the
Go packages and the NixOS module shared by
[microvm-github-runners] (throwaway runner microVMs) and [ai-workspace]
(persistent workspace VMs). vmkit is not a general cloud-hypervisor SDK; it
is the part of those two that does not care which VMs it runs.

## Go packages

```
go get github.com/OSSystems/vmkit-nix
```

| Package | What it does |
|---|---|
| `ch` | The cloud-hypervisor command line from a typed `VM` (kernel or firmware boot, disks, taps, vsock, serial, virtiofs shares, virtio-mem); the REST API client (`Info`, `ResizeMemory`, `PowerButton`); `DialGuest`, the host side of the vsock `CONNECT` handshake. |
| `vsock` | The guest side of vsock: `Dial` the host, `Listen` for it. |
| `control` | The guest-to-host control channel: one JSON request line, one reply line, and the `stats` request the memory policy reads. Callers add their own operations by embedding `Reply`. |
| `memory` | The elastic memory policy: guests boot small and grow or shrink 1 GiB at a time through virtio-mem by their memory pressure, keeping the host's reserve. `Status` is what one pool of VMs publishes so the other pools on the host account for it. |
| `virtiofs` | virtiofsd arguments, and `StoreView`: a store holding exactly one Nix closure, so a guest sees its system and nothing else of the host's store. |
| `tap` | Persistent taps an unprivileged cloud-hypervisor can open. |
| `hostutil` | Accounts, systemd unit state, load, meminfo, atomic writes. |

vmkit never keeps a VM as its child: `ch.VM.Args` is a command line, and the
caller decides whether to exec it as a systemd unit's main process or
supervise it.

Linux only, no cgo, no dependencies outside the standard library.

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
  microvm-github-runners.inputs.vmkit.follows = "vmkit";
  ai-workspace.inputs.vmkit.follows = "vmkit";
};
```

## Development

```
nix flake check   # go test, and the module's evaluation tests
nix fmt -- $(git ls-files '*.nix')
```

[cloud-hypervisor]: https://github.com/cloud-hypervisor/cloud-hypervisor
[microvm-github-runners]: https://github.com/OSSystems/microvm-github-runners
[ai-workspace]: https://github.com/OSSystems/ai-workspace
