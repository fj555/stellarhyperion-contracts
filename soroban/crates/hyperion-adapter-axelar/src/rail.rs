//! Axelar's Interchain Token Service, as this adapter calls it.
//!
//! Declared here rather than pulled in as a dependency, for the same reason Circle's contracts
//! are. Axelar's Stellar packages are not on crates.io, and vendoring a git revision of somebody
//! else's workspace into a contract build is a supply chain decision rather than a convenience.
//! Soroban resolves a cross contract call by function name and argument shape at invocation time,
//! so a transcription that has drifted fails immediately and loudly on the first call rather than
//! quietly doing the wrong thing.
//!
//! # Message and Trust Surfaces: GMP vs ITS Token-Manager Transitions
//!
//! Axelar routes encompass two trust and message boundaries:
//! 1. **ITS Token-Manager transitions**: Native interchain token transfers, mint/burn, and lock/unlock
//!    managers authenticate token operations and gas relaying. Token mappings are verified against ITS
//!    at registration (`map_token`), ensuring asset alignment.
//! 2. **GMP message transitions**: Payloads attached to transfers specify the far-side recipient and
//!    origin router metadata. Inbound execution (`execute_with_interchain_token`) strictly enforces
//!    that the source caller matches ITS and the originating payload sender matches the trusted `peer`
//!    configured for the source chain.
//!
//! # Source-Note and Destination-Payment Reconciliation
//!
//! Axelar paid ITS dispatch authenticates the sender and native gas payment. Reconciling cross-chain
//! dispatches requires joining encoded note intent with candidate destination ITS payment events:
//! - **Exact agreement criteria**:
//!   1. Recipient decoded from source note must match the destination payment recipient.
//!   2. Axelar token ID must match between source dispatch and destination receipt.
//!   3. Dispatched amount and destination received amount must match exactly as integers (no float math).
//!   4. Full command identity (e.g. Axelar `commandId` or `messageId` reference) must be present on both records and agree exactly.
//! - **Unverified reporting**: Partial receipts (missing amount, recipient, or command identity) or
//!   mismatched receipts are reported as unverified development evidence with explicit discrepancy tags.
//! - **Raw reference retention**: Reconciliation tools must retain original raw references (transaction
//!   hashes, block numbers, raw note payloads, token addresses) without relying on display nonces for
//!   correctness.

//!
//! These comparisons are development evidence only: no event authentication, finality, custody or
//! execution proof is established. Source and candidate raw references remain separate when keys
//! overlap. See `docs/AXELAR_RECONCILIATION.md` for the report boundary and compatibility details.
//!
//! # In-Flight Message Transition and Delayed Rotation
//!
//! When updating trusted peers on a connected chain:
//! - **Delay enforcement**: A proposed peer rotation requires a minimum delay of
//!   [`MIN_PEER_ROTATION_DELAY`] (1 hour) before it can be applied. The new peer cannot deliver any
//!   messages during this timelock window.
//! - **In-flight deliveries**: Dispatches initiated from the currently trusted peer continue to be
//!   accepted during the delay period.
//! - **Cutover**: Once `apply_peer_rotation` is invoked after the delay, the active peer is updated.
//!   The former peer is immediately invalidated; subsequent deliveries claiming the old peer fail
//!   closed with `UnexpectedRailContract`.
//! - **Emergency disable**: `set_chain_enabled(chain, false)` immediately halts all outbound dispatches
//!   and inbound payload deliveries (`RouteDisabled`), preventing unauthorized message processing.
//! - **Unlink**: `unlink_chain(chain)` permanently retires both chain names and removes active
//!   mappings, halting inbound deliveries with `UnknownChain`. Retired names cannot be relinked.
//! - **Setup safety**: `link_chain` permits initial or identical links, never an immediate peer
//!   replacement or reverse-name collision. Pending proposals must be cancelled before replacement.
//! - **Lifetime**: pending, disable and retirement metadata shares the adapter instance lifetime.
//!   Renewing or restoring peer mappings cannot silently re-enable or change trust.
//!
//! # Deployment Gating
//!
//! The active deployment is Stellar Testnet outbound ITS/CCTP. EVM contracts, GMP inbound execution,
//! and other rail development and subsequent peer migrations remain explicitly gated until
//! independently validated on testnets.
//! # Paid Dispatch Authorization Trees and Privilege Expansion
//!
//! Axelar paid ITS dispatch authenticates the sender and native gas payment in a single atomic authorization
//! tree signed by the source wallet:
//! - **Nested authorization structure**:
//!   1. Root invocation: `router.bridge_out_with_gas(user, request, gas_amount)`
//!   2. Fee debit: `token.transfer(user, treasury, fee_amount)`
//!   3. Adapter dispatch: `adapter.dispatch_from(router, user, token, net_amount, destination_chain, destination, nonce, gas_amount)`
//!      - ITS transfer: `its.interchain_transfer(user, token_id, destination_chain, recipient, net_amount, None, gas_token)`
//!        - Principal debit: `token.burn(user, net_amount)` (for mint/burn managers) OR `token.transfer(user, manager, net_amount)` (for lock/unlock)
//!        - Gas debit: `gas_token.transfer(user, its_gas_recipient, gas_amount)`
//!
//! - **Privilege expansion protection**:
//!   Before signing, the simulated authorization tree must be verified against expected snapshot invariants:
//!   - **Spender identity**: Only the configured treasury, registered token manager, or token burn may be authorized.
//!   - **Gas recipient**: Only the trusted ITS contract or native gas service may receive the gas payment.
//!   - **Exact integer amounts**: Principal amount, router fee, and gas payment must match exact integers without inflation.
//!   - **Zero unapproved calls**: Any unexpected sub-invocation (e.g. extraneous token transfers, approvals, or admin calls)
//!     is treated as a critical privilege escalation and rejected.
//!
//! - **Offline snapshot inspection**:
//!   Snapshot generation and inspection require no private keys and no network broadcast. Simulation footprints and
//!   authorization specs are verifiable entirely offline or through simulated view calls.
//!
//! # Deployment Gating
//!
//! The active deployment is Stellar Testnet outbound ITS/CCTP. EVM contracts, GMP inbound execution, and other rail
//! development remain strictly gated until independently validated.
//!
//! Transcribed from `axelarnetwork/axelar-amplifier-stellar` on branch `main`:
//!
//! - `contracts/stellar-interchain-token-service/src/interface.rs`
//! - `contracts/stellar-interchain-token-service/src/types.rs`
//! - `packages/stellar-axelar-std/src/types.rs`

pub use hyperion_core::MIN_PEER_ROTATION_DELAY;

use soroban_sdk::{contractclient, contracttype, Address, Bytes, BytesN, Env, String};

pub use hyperion_core::axelar::TokenManagerType;

/// What Axelar charges gas in, and how much of it.
///
/// Paid transfers attach this exact amount through ITS; the source account authorizes it.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Token {
    pub address: Address,
    pub amount: i128,
}

#[contractclient(name = "InterchainTokenServiceClient")]
pub trait InterchainTokenService {
    fn native_token_address(env: Env) -> Address;
    fn gas_service(env: Env) -> Address;
    /// Send an interchain token to another chain, optionally with a payload for the recipient.
    ///
    /// `caller` pays: ITS burns from or transfers out of that address, so whoever is named here
    /// has to have authorised the movement. `metadata` carries the payload that turns a plain
    /// transfer into a contract call on the far side, and `gas_token` is the relayer fee.
    #[allow(clippy::too_many_arguments)]
    fn interchain_transfer(
        env: Env,
        caller: Address,
        token_id: BytesN<32>,
        destination_chain: String,
        destination_address: Bytes,
        amount: i128,
        metadata: Option<Bytes>,
        gas_token: Option<Token>,
    );

    /// Whether Axelar's hub will route to this chain at all.
    fn is_trusted_chain(env: Env, chain: String) -> bool;

    /// The local asset a token id resolves to.
    ///
    /// Read at configuration time so a mapping that disagrees with Axelar is caught once, by an
    /// admin, rather than on every dispatch as a panic from two contracts away.
    fn registered_token_address(env: Env, token_id: BytesN<32>) -> Address;

    /// The token manager holding the locked side of a `LockUnlock` token id.
    fn deployed_token_manager(env: Env, token_id: BytesN<32>) -> Address;

    /// How ITS will take the funds, which decides what this adapter has to authorise.
    fn token_manager_type(env: Env, token_id: BytesN<32>) -> TokenManagerType;

    /// Axelar's own flow limit on a token id, if its operator set one.
    fn flow_limit(env: Env, token_id: BytesN<32>) -> Option<i128>;
}
