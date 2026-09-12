//! Device trust, pairing, recovery and encrypted key storage reused from PR #252.
//! Credential sharing uses this vault independently of workspace transport rollout.
pub mod client;
pub mod service;
pub mod store;
pub use service::{VaultPhase, VaultService, VaultStatus, object_id_for};
pub use store::{
    LockedProtection, MemoryProtection, ProtectionKeyProvider, VaultStore, platform_protection,
};
