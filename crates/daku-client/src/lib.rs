//! Rust transport and lifecycle support for clients of `daku-daemon`.

mod client;
pub mod persistence;
mod process;

pub use client::DaemonClient;
pub use daku_protocol::identity;
pub use process::DaemonSupervisor;
