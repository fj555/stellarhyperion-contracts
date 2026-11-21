use soroban_sdk::{contracttype, Address, BytesN, String};

/// The finality requirement selected for one CCTP destination lane.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FinalityPolicy {
    Finalized,
    Fast,
}

impl FinalityPolicy {
    pub fn threshold(&self) -> u32 {
        match self {
            Self::Finalized => hyperion_core::cctp::FINALITY_THRESHOLD_FINALIZED,
            Self::Fast => hyperion_core::cctp::FINALITY_THRESHOLD_FAST,
        }
    }
}

/// Lane-specific values shown to callers before a CCTP dispatch.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DispatchQuote {
    pub destination_domain: u32,
    pub max_fee: i128,
    pub min_finality_threshold: u32,
}

/// Everything this adapter was told once and then relies on.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Config {
    /// In production this is the same multisig that holds the router's admin role. It is kept
    /// separate in storage rather than read back from the router so that a router upgrade cannot
    /// silently move who controls the rail wiring.
    pub admin: Address,
    pub router: Address,
    /// `token-messenger-minter-v2`. Burns on the way out, mints on the way in, and is the
    /// `recipient` named inside every burn message that lands here.
    pub token_messenger: Address,
    /// `message-transmitter-v2`. Checks Circle's signatures.
    pub message_transmitter: Address,
    /// Ceiling on the fee Circle may take, as basis points of the amount being sent.
    ///
    /// Finalized lanes do not use this budget. A fast lane must be explicitly configured and
    /// requires a nonzero budget.
    pub max_fee_bps: u32,
    /// Legacy configuration field retained for serialization compatibility. It remains at 2000;
    /// the effective finality is stored per lane.
    ///
    /// 2000 means "wait for real finality".
    pub min_finality_threshold: u32,
}

/// A chain Hyperion knows how to reach over CCTP, in both directions.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DomainLink {
    /// The name the router and the app use, for example `ethereum` or `base`.
    pub chain: String,
    /// Circle's own numbering. Stellar is 27.
    pub domain: u32,
    /// Who may broadcast the resulting message on the far side. All zeroes means anybody.
    pub destination_caller: BytesN<32>,
}

/// A proposed peer messenger rotation on a remote domain awaiting the timelock delay.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PeerMessengerRotation {
    /// The new remote messenger address on the source domain.
    pub new_messenger: BytesN<32>,
    /// Timestamp at or after which `apply` may be called.
    pub eta: u64,
    /// Timestamp when the rotation was proposed.
    pub queued_at: u64,
}

/// An active overlap acceptance window for historical messages from a retired peer messenger.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoricalMessengerWindow {
    /// The retired remote messenger address.
    pub old_messenger: BytesN<32>,
    /// Timestamp until which historical messages from `old_messenger` are still accepted.
    pub valid_until: u64,
}

/// A proposed local TokenMessenger rotation awaiting the timelock delay.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessengerRotation {
    /// The replacement TokenMessenger address proposed by the admin.
    pub new_messenger: Address,
    /// Timestamp at or after which `apply` may be called.
    pub eta: u64,
    /// Timestamp when the rotation was proposed.
    pub queued_at: u64,
}

/// Historical window for retired local TokenMessenger.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoricalTokenMessengerWindow {
    /// The retired local TokenMessenger address.
    pub old_messenger: Address,
    /// Timestamp until which messages naming `old_messenger` are still accepted.
    pub valid_until: u64,
}
