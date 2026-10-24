//! Rust networking core for the Windows AirDrop port.

pub mod airdrop;
#[cfg(windows)]
pub mod ble;
pub mod cancel;
pub mod discovery;
pub mod identity;
pub mod mdns;
pub mod outcome;
pub mod receiver;
pub mod sender;
pub mod tls;

static BOARD_FEEDBACK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Enable optional ESP32 status reporting through the Rust bridge process.
/// A missing display never prevents a transfer; library callers opt in explicitly.
pub fn enable_board_feedback() {
    BOARD_FEEDBACK.store(true, std::sync::atomic::Ordering::Relaxed);
}

pub(crate) fn board_feedback_enabled() -> bool {
    BOARD_FEEDBACK.load(std::sync::atomic::Ordering::Relaxed)
}

pub use airdrop::{IncomingFile, IncomingTransfer};
pub use cancel::SendCancellation;
pub use discovery::{
    discover_receivers_live, discover_receivers_live_cancellable, receiver_by_name, DiscoveryEvent,
};
pub use identity::{Identity, IdentityError};
pub use mdns::{AirDropService, MdnsBrowser, MdnsResponder, NetworkInterface};
pub use outcome::TransferFailureKind;
pub use receiver::{ReceiverCore, ReceiverError, ReceiverEvent, ReceiverOptions, TransferFormat};
pub use sender::{
    discover_receivers, send_to_name, send_to_receiver, send_to_receiver_cancellable,
    validate_files, NearbyReceiver, SendError, SendEvent, SendFormat, SendOptions, SendReport,
};
pub use tls::{server_config, TlsError};
