//! Drives cloud-hypervisor: its command line, its REST API over the API
//! socket, and the host side of its vsock device.

mod client;
mod vm;
mod vsock;

pub use client::{Client, Error, VmInfo};
pub use vm::{Disk, Net, Share, Vm};
pub use vsock::{dial_guest, guest_port_socket};
