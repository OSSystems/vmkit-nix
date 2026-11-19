//! Building blocks for running VMs on [cloud-hypervisor] from NixOS hosts:
//! the part of a controller that does not care which VMs it runs,
//! throwaway or long-lived.
//!
//! vmkit never keeps a VM as its child: [`ch::Vm::args`] is a command line,
//! and the caller decides whether to exec it as a systemd unit's main
//! process or supervise it. I/O is async on tokio; what only computes or
//! touches files is plain functions.
//!
//! What vmkit's processes exchange with each other and with guests is
//! described in `docs/contracts.md`.
//!
//! [cloud-hypervisor]: https://www.cloudhypervisor.org/

#![cfg(target_os = "linux")]

pub mod ch;
pub mod hostutil;
