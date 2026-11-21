use hyperion_core::RouteKind;
use soroban_sdk::{contracttype, Address, BytesN, String};

/// Who this adapter works for, and which of the two Axelar shapes it speaks.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Config {
    /// Kept separate in storage rather than read back from the router, so that a router upgrade
    /// cannot silently move who controls the rail wiring.
    pub admin: Address,
    pub router: Address,
    pub its: Address,
    /// `AxelarIts` for a bare transfer, `AxelarGmp` for one carrying a Hyperion note. One WASM,
    /// two instances, and an instance never changes its mind about which it is.
    pub route: RouteKind,
}

/// One lane, in Hyperion's vocabulary and in Axelar's.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChainLink {
    /// What Hyperion calls the chain, which is what a user sees and what the router carries.
    pub chain: String,
    /// What Axelar calls it. The two agree often enough to be tempting and differ often enough
    /// to be a bug, so both are written down.
    pub axelar_chain: String,
    /// Hyperion's own contract over there. Only the GMP shape needs one, and on that shape it is
    /// the single address an inbound note is accepted from.
    pub peer: BytesN<20>,
}

/// A proposed peer rotation awaiting the timelock delay to elapse.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingRotation {
    /// The newly proposed peer contract on the remote chain.
    pub new_peer: BytesN<20>,
    /// Ledger timestamp when this rotation can first be applied.
    pub eta: u64,
    /// Ledger timestamp when the rotation was proposed.
    pub queued_at: u64,
}

/// Expected parameters for a paid dispatch authorization tree, readable offline.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaidDispatchAuthSpec {
    pub token: Address,
    pub amount: i128,
    pub token_manager: Address,
    pub takes_by_burning: bool,
    pub gas_token: Address,
    pub gas_recipient: Address,
    pub gas_amount: i128,
}
