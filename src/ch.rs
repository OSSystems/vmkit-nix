//! Drives cloud-hypervisor: its command line, its REST API over the API
//! socket, and the host side of its vsock device.

mod client;
mod vm;

pub use client::{Client, Error, VmInfo};
pub use vm::{Disk, Net, Share, Vm};
