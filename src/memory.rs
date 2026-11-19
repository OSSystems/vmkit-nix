//! vmkit's elastic memory policy: guests boot small and grow or shrink
//! 1 GiB at a time through virtio-mem, by their memory pressure, as long as
//! the host keeps its reserve. Pools of VMs sharing a host publish a
//! [`Status`] so each accounts for the others.
//!
//! Sizes are bytes in `i64`: what may still be handed out goes negative
//! when the host is past its reserve.

mod policy;
mod status;

pub use policy::*;
pub use status::*;
