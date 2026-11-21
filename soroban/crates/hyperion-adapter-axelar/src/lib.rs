//! Hyperion's adapter for Axelar, covering both shapes Stellar's Interchain Token Service
//! offers.
//!
//! Axelar is the general purpose rail. A validator set watches the source chain, signs off on
//! what it saw, and a gateway on the destination chain replays it. The Interchain Token Service
//! sits on top of that and knows about tokens: a token id has a manager on each chain, and a
//! transfer either burns and mints or locks and unlocks depending on how that manager was set
//! up. None of that is Hyperion's to verify. This adapter is the two thin edges.
//!
//! # One WASM, two shapes
//!
//! An instance is wired at `initialize` for exactly one route and never changes its mind.
//!
//! `AxelarIts` is the bare shape. A transfer goes out naming the recipient on the far side
//! directly, and Axelar delivers it there with nothing else attached. Nothing comes back the
//! other way, because a bare transfer arriving on Stellar is delivered by ITS straight to the
//! address it names and Hyperion is never in the path at all. The router's inbound receiver for
//! this route stays unset, so `bridge_in` fails closed on it.
//!
//! `AxelarGmp` is the shape that carries a note. Outbound, the transfer is addressed to
//! Hyperion's own contract on the far side and the real recipient travels in the payload.
//! Inbound, ITS calls `execute_with_interchain_token` here and this contract hands the funds to
//! the router.
//!
//! That second shape is worth its extra moving parts for one reason. A bare ITS transfer to a
//! Stellar account with no trustline for the asset reverts the whole Axelar execution and
//! strands the message; there is no partial success and no retry that will ever work. Routing
//! through Hyperion instead lets the router park the delivery as a claim that the recipient can
//! settle themselves once they have opened the trustline. The same argument covers muxed
//! accounts, which the bare shape has nowhere to put and this one carries fine.
//!
//! # Relay gas
//!
//! `dispatch_from` takes gas and net tokens from the user's authenticated account through ITS.
//! The source wallet signs the simulated authorization tree; no copy of Axelar's payload is
//! needed to authorize its gas service. The legacy `dispatch` interface carries no gas payment.
//!
//! # What this contract does not do
//!
//! It does not verify anything Axelar signed. It never sees a signature. On the way in it is
//! called by ITS and its entire claim to trust is that ITS is the direct invoker, which the
//! host proves rather than this code.

#![no_std]

mod events;
mod storage;

// Public so that the rail interface this adapter was built against is readable from outside it,
// and so the deploy tooling can generate bindings for the same shapes rather than a second
// transcription of them.
pub mod rail;
pub mod types;

#[cfg(test)]
mod test;

use hyperion_core::{
    address::{assert_route_supports, bytes32_to_evm, evm_to_bytes32},
    axelar::{
        negotiate_note_version, DestinationCapability, DestinationCapabilityRecord, InboundNote,
        OutboundNote, SUPPORTED_NOTE_VERSIONS,
    },
    inbound::{Origin, Recipient, RouterClient},
    AdapterKeyCoverage, HyperionError, MaintenanceSummary, RouteKind, TokenMappingConflict,
    TokenMappingReport, MAX_MAINTENANCE_BATCH, MIN_PEER_ROTATION_DELAY,
};
use soroban_sdk::{
    auth::{ContractContext, InvokerContractAuthEntry, SubContractInvocation},
    contract, contractimpl, token, vec,
    xdr::ToXdr,
    Address, Bytes, BytesN, Env, IntoVal, String, Symbol, Vec,
};

use rail::{InterchainTokenServiceClient, Token, TokenManagerType};
use types::{ChainLink, Config, PaidDispatchAuthSpec, PendingRotation};

#[contract]
pub struct AxelarAdapter;

#[contractimpl]
impl AxelarAdapter {
    /// Wire the adapter up and fix which of the two shapes this instance speaks.
    ///
    /// Starts with no lane linked and no token mapped, so a half finished deploy script leaves
    /// something that refuses every transfer rather than something that guesses.
    pub fn initialize(
        env: Env,
        admin: Address,
        router: Address,
        its: Address,
        route: RouteKind,
    ) -> Result<(), HyperionError> {
        if storage::is_initialized(&env) {
            return Err(HyperionError::AlreadyInitialized);
        }
        // The same code backs both Axelar routes and nothing else. Handing it `Cctp` is a
        // deployment mistake, and one that would otherwise sit quiet until the router refused
        // to talk to it.
        if !matches!(route, RouteKind::AxelarIts | RouteKind::AxelarGmp) {
            return Err(HyperionError::UnsupportedRoute);
        }
        admin.require_auth();
        storage::set_config(
            &env,
            &Config {
                admin,
                router,
                its,
                route,
            },
        );
        Ok(())
    }

    // ---------------------------------------------------------------------------------------
    // Outbound
    // ---------------------------------------------------------------------------------------

    // Auth-free validation only. Dispatch keeps caller/sender authorization unchanged.
    pub fn check_dispatch(
        env: Env,
        caller: Address,
        token: Address,
        amount: i128,
        destination_chain: String,
        destination: BytesN<32>,
        gas_amount: Option<i128>,
    ) -> Result<(), HyperionError> {
        validate_dispatch(
            &env,
            &caller,
            &token,
            amount,
            &destination_chain,
            &destination,
            gas_amount,
        )?;
        Ok(())
    }

    /// Hand funds the router has already moved here over to ITS.
    ///
    /// The router transfers the net amount in before calling, so by the time this runs the
    /// money is already sitting in this contract's balance and the only question is how ITS
    /// wants to take it.
    pub fn dispatch(
        env: Env,
        caller: Address,
        token: Address,
        amount: i128,
        destination_chain: String,
        destination: BytesN<32>,
        nonce: u64,
    ) -> Result<(), HyperionError> {
        caller.require_auth();
        let (cfg, link, token_id, recipient) = validate_dispatch(
            &env,
            &caller,
            &token,
            amount,
            &destination_chain,
            &destination,
            None,
        )?;
        if cfg.route == RouteKind::AxelarGmp {
            let required = u8::try_from(storage::required_note_version(&env, &destination_chain))
                .map_err(|_| HyperionError::UnsupportedHookVersion)?;
            negotiate_note_version(required, &SUPPORTED_NOTE_VERSIONS, &SUPPORTED_NOTE_VERSIONS)?;
        }

        let this = env.current_contract_address();
        let its = InterchainTokenServiceClient::new(&env, &cfg.its);
        let manager_type = require_destination_capability(&env, &cfg, &link, &token_id)?;

        // Ask ITS how it intends to take the funds rather than authorising every way it might.
        // Three of the four manager types burn straight out of this balance and only
        // `LockUnlock` moves them somewhere, so one read here buys an authorisation that names
        // exactly one call instead of a pair covering both.
        let context = if manager_type.takes_by_burning() {
            ContractContext {
                contract: token.clone(),
                fn_name: Symbol::new(&env, "burn"),
                args: (this.clone(), amount).into_val(&env),
            }
        } else {
            ContractContext {
                contract: token.clone(),
                fn_name: Symbol::new(&env, "transfer"),
                args: (this.clone(), its.deployed_token_manager(&token_id), amount).into_val(&env),
            }
        };
        // Nothing wider than that. One contract, one function, one exact argument list, and no
        // sub-invocations underneath it.

        // On the bare shape ITS delivers to the recipient itself. On the shape that carries a
        // note it delivers to Hyperion's contract over there, and the recipient rides along in
        // the payload with the router's nonce for company.
        let (destination_address, note) = match cfg.route {
            RouteKind::AxelarGmp => (
                Bytes::from_array(&env, &link.peer.to_array()),
                Some(OutboundNote::new(recipient, nonce).encode(&env)?),
            ),
            _ => (Bytes::from_array(&env, &recipient.to_array()), None),
        };

        let asset = token::Client::new(&env, &token);
        let sender_before = asset.balance(&this);
        let manager = if manager_type.takes_by_burning() {
            this.clone()
        } else {
            its.deployed_token_manager(&token_id)
        };
        let manager_before = if manager_type.takes_by_burning() {
            0
        } else {
            asset.balance(&manager)
        };
        env.authorize_as_current_contract(vec![
            &env,
            InvokerContractAuthEntry::Contract(SubContractInvocation {
                context,
                sub_invocations: vec![&env],
            }),
        ]);

        its.interchain_transfer(
            &this,
            &token_id,
            &link.axelar_chain,
            &destination_address,
            &amount,
            &note,
            // No gas token. See the note at the top of this file: the keeper pays for the
            // message once the gateway has emitted it.
            &None,
        );
        let expected_debit = amount;
        hyperion_core::amount::assert_exact_transfer(
            asset.balance(&this),
            sender_before,
            expected_debit,
        )?;
        if !manager_type.takes_by_burning() {
            hyperion_core::amount::assert_exact_transfer(
                manager_before,
                asset.balance(&manager),
                amount,
            )?;
        }

        events::Dispatched {
            token,
            destination_chain,
            amount,
            token_id,
            axelar_chain: link.axelar_chain,
            destination_address,
            token_manager_type: manager_type,
            note,
            router_nonce: nonce,
        }
        .publish(&env);
        Ok(())
    }

    /// The native token ITS accepts for relay gas.
    pub fn gas_token(env: Env) -> Result<Address, HyperionError> {
        let cfg = storage::config(&env)?;
        Ok(InterchainTokenServiceClient::new(&env, &cfg.its).native_token_address())
    }

    /// ITS takes net tokens and gas directly from the authenticated sender. This lets the
    /// source account authorize ITS's actual nested gas call, without duplicating its payload.
    #[allow(clippy::too_many_arguments)]
    pub fn dispatch_from(
        env: Env,
        caller: Address,
        sender: Address,
        token: Address,
        amount: i128,
        destination_chain: String,
        destination: BytesN<32>,
        nonce: u64,
        gas_amount: i128,
    ) -> Result<(), HyperionError> {
        caller.require_auth();
        sender.require_auth();
        let (cfg, link, token_id, recipient) = validate_dispatch(
            &env,
            &caller,
            &token,
            amount,
            &destination_chain,
            &destination,
            Some(gas_amount),
        )?;
        let its = InterchainTokenServiceClient::new(&env, &cfg.its);
        let manager_type = require_destination_capability(&env, &cfg, &link, &token_id)?;
        let destination_address = Bytes::from_array(&env, &recipient.to_array());
        let asset = token::Client::new(&env, &token);
        let sender_before = asset.balance(&sender);
        let manager = if manager_type.takes_by_burning() {
            sender.clone()
        } else {
            its.deployed_token_manager(&token_id)
        };
        let manager_before = if manager_type.takes_by_burning() {
            0
        } else {
            asset.balance(&manager)
        };
        its.interchain_transfer(
            &sender,
            &token_id,
            &link.axelar_chain,
            &destination_address,
            &amount,
            &None,
            &Some(Token {
                address: its.native_token_address(),
                amount: gas_amount,
            }),
        );
        let mut expected_debit = amount;
        if its.native_token_address() == token {
            expected_debit = amount
                .checked_add(gas_amount)
                .ok_or(HyperionError::DecimalOverflow)?;
        }
        hyperion_core::amount::assert_exact_transfer(
            asset.balance(&sender),
            sender_before,
            expected_debit,
        )?;
        if !manager_type.takes_by_burning() {
            hyperion_core::amount::assert_exact_transfer(
                manager_before,
                asset.balance(&manager),
                amount,
            )?;
        }

        events::Dispatched {
            token,
            destination_chain,
            amount,
            token_id: token_id.clone(),
            axelar_chain: link.axelar_chain,
            destination_address,
            token_manager_type: manager_type,
            note: None,
            router_nonce: nonce,
        }
        .publish(&env);
        Ok(())
    }

    // ---------------------------------------------------------------------------------------
    // Inbound
    // ---------------------------------------------------------------------------------------

    /// ITS's callback for a transfer that arrived carrying a payload.
    ///
    /// The name and the argument list are Axelar's, not Hyperion's: ITS looks this up by name
    /// on the address the transfer was addressed to. Returning the router's claim id rather
    /// than nothing is harmless, because ITS discards whatever comes back, and it means the
    /// same value an indexer needs is in the return as well as the event.
    ///
    /// # Authorisation
    ///
    /// Only ITS can get here. The `require_auth` below is satisfied implicitly because ITS is
    /// the direct invoker, and there is no other way to satisfy it: an account cannot sign for
    /// a contract address. Anybody else calling this is refused before a single check runs.
    ///
    /// ITS gives the tokens to this contract *before* it makes this call, so the funds are
    /// already here. That ordering is Axelar's and this contract checks it rather than assuming
    /// it, because a callback that hands the router a number its balance does not back would
    /// park a claim against money nobody holds.
    #[allow(clippy::too_many_arguments)]
    pub fn execute_with_interchain_token(
        env: Env,
        source_chain: String,
        message_id: String,
        source_address: Bytes,
        payload: Bytes,
        token_id: BytesN<32>,
        token_address: Address,
        amount: i128,
    ) -> Result<u64, HyperionError> {
        let cfg = storage::config(&env)?;
        cfg.its.require_auth();
        // The bare shape has no inbound leg to speak of. An instance wired for it that somehow
        // finds itself being called back is misconfigured, and saying so is better than
        // quietly growing a second personality.
        if cfg.route != RouteKind::AxelarGmp {
            return Err(HyperionError::UnsupportedRoute);
        }
        if amount <= 0 {
            return Err(HyperionError::InvalidAmount);
        }

        // Axelar's name for the chain, back to Hyperion's. An unrecognised source is refused
        // here, which is what happens on the day somebody deploys a Hyperion peer on a chain
        // this instance has never been told about.
        let chain = storage::chain_of(&env, &source_chain).ok_or(HyperionError::UnknownChain)?;
        if storage::is_chain_disabled(&env, &chain) {
            return Err(HyperionError::RouteDisabled);
        }
        let link = storage::link(&env, &chain).ok_or(HyperionError::UnknownChain)?;

        // ITS will deliver a payload from anybody who pays for one. Only Hyperion's own
        // contract on that chain is allowed to send instructions here.
        let peer = Bytes::from_array(&env, &link.peer.to_array());
        if source_address != peer {
            return Err(HyperionError::UnexpectedRailContract);
        }

        // Two names for one asset, and they have to agree. The mapping is also the allowlist,
        // so a token id nobody registered here stops at this line.
        let mapped = storage::token_of(&env, &token_id).ok_or(HyperionError::TokenNotMapped)?;
        if mapped != token_address
            || storage::token_id(&env, &token_address).as_ref() != Some(&token_id)
        {
            return Err(HyperionError::TokenNotMapped);
        }
        // ITS is already on the callback stack: validate its admitted record without re-entering ITS.
        admitted_destination_capability(&env, &cfg, &link, &token_id)?;

        let note = InboundNote::decode(&env, &payload)?;
        assert_route_supports(cfg.route, note.destination.kind)?;
        let recipient = note.destination.to_address(&env)?;

        // Measure rather than believe. ITS says it gave this contract the funds; the balance is
        // what decides whether it did.
        let this = env.current_contract_address();
        let asset = token::Client::new(&env, &token_address);
        if asset.balance(&this) < amount {
            return Err(HyperionError::NothingMinted);
        }

        // The router pulls rather than trusts an adapter's word, so say in advance that this
        // one transfer is authorised. Nothing wider: one contract, one function, one exact
        // argument list, and no sub-invocations underneath it.
        env.authorize_as_current_contract(vec![
            &env,
            InvokerContractAuthEntry::Contract(SubContractInvocation {
                context: ContractContext {
                    contract: token_address.clone(),
                    fn_name: Symbol::new(&env, "transfer"),
                    args: (this.clone(), cfg.router.clone(), amount).into_val(&env),
                },
                sub_invocations: vec![&env],
            }),
        ]);

        let claim_id = RouterClient::new(&env, &cfg.router).bridge_in(
            &this,
            &cfg.route,
            &token_address,
            &amount,
            &Recipient {
                address: recipient.clone(),
                kind: note.destination.kind,
                raw: payload,
            },
            &Origin {
                chain,
                nonce: note.nonce,
                message_id: replay_key(&env, &source_chain, &message_id),
                sender: evm_to_bytes32(&env, &link.peer),
            },
        );

        events::Received {
            token: token_address,
            source_chain,
            amount,
            token_id,
            message_id,
            recipient,
            nonce: note.nonce,
            claim_id,
        }
        .publish(&env);
        Ok(claim_id)
    }

    // ---------------------------------------------------------------------------------------
    // Housekeeping
    // ---------------------------------------------------------------------------------------

    /// Move any stray balance to the protocol treasury.
    ///
    /// An adapter should hold nothing between transactions, but rounding on a lock and unlock
    /// token, or a transfer somebody sent here by hand, can leave a residue. An admin only
    /// rescue would be a standing power to move tokens out of a contract users send tokens to,
    /// which is a trust surface nobody needs for the sake of dust. Instead this is open to
    /// anybody and the destination is read live from the router, so the worst a caller can do
    /// is pay a fee to tidy up.
    pub fn sweep(env: Env, caller: Address, token: Address) -> Result<i128, HyperionError> {
        caller.require_auth();
        let cfg = storage::config(&env)?;
        let this = env.current_contract_address();
        let asset = token::Client::new(&env, &token);
        let balance = asset.balance(&this);
        if balance <= 0 {
            return Ok(0);
        }
        let treasury = RouterClient::new(&env, &cfg.router).treasury();
        asset.transfer(&this, &treasury, &balance);
        events::Swept {
            token,
            amount: balance,
            treasury,
            caller,
        }
        .publish(&env);
        Ok(balance)
    }

    /// Keep a quiet lane's storage from being archived. Permissionless, changes nothing.
    pub fn keep_alive(env: Env, chain: String) -> Result<(), HyperionError> {
        let _ = storage::config(&env)?;
        env.storage()
            .instance()
            .extend_ttl(storage::BUMP_THRESHOLD, storage::BUMP_TO);
        let _ = storage::touch_link(&env, &chain);
        Ok(())
    }

    /// Bounded storage maintenance for Axelar peer trust (`Link`, `Source`) and token mappings
    /// (`TokenId`, `Token`).
    pub fn maintain_storage(
        env: Env,
        tokens: Vec<Address>,
        peer_chains: Vec<String>,
        peer_domains: Vec<u32>,
        remote_tokens: Vec<BytesN<32>>,
        inbound_messages: Vec<BytesN<32>>,
    ) -> Result<MaintenanceSummary, HyperionError> {
        let _ = storage::config(&env)?;
        if tokens.len() > MAX_MAINTENANCE_BATCH
            || peer_chains.len() > MAX_MAINTENANCE_BATCH
            || peer_domains.len() > MAX_MAINTENANCE_BATCH
            || remote_tokens.len() > MAX_MAINTENANCE_BATCH
            || inbound_messages.len() > MAX_MAINTENANCE_BATCH
            || peer_chains
                .len()
                .saturating_add(tokens.len())
                .saturating_add(remote_tokens.len())
                .saturating_mul(2)
                > MAX_MAINTENANCE_BATCH
            || !peer_domains.is_empty()
            || !inbound_messages.is_empty()
        {
            return Err(HyperionError::InvalidLimit);
        }
        env.storage()
            .instance()
            .extend_ttl(storage::BUMP_THRESHOLD, storage::BUMP_TO);

        let mut peer_trust_touched = 0u32;
        for chain in peer_chains.iter() {
            peer_trust_touched += storage::touch_link(&env, &chain);
        }

        let mut token_mappings_touched = 0u32;
        for token in tokens.iter() {
            token_mappings_touched += storage::touch_token(&env, &token);
        }
        for token_id in remote_tokens.iter() {
            token_mappings_touched += storage::touch_remote_token(&env, &token_id);
        }

        Ok(MaintenanceSummary {
            instance_extended: true,
            token_mappings_touched,
            peer_trust_touched,
            inbound_records_touched: 0,
            total_keys_touched: peer_trust_touched + token_mappings_touched,
        })
    }

    /// Explicit key coverage for Axelar adapter storage maintenance.
    pub fn key_coverage(env: Env) -> Result<AdapterKeyCoverage, HyperionError> {
        let cfg = storage::config(&env)?;
        Ok(AdapterKeyCoverage {
            route: cfg.route,
            has_token_mappings: true,
            has_peer_trust: true,
            has_inbound_records: false,
            token_mapping_keys: 2,
            peer_trust_keys: 2,
            inbound_record_keys: 0,
        })
    }

    // ---------------------------------------------------------------------------------------
    // Administration
    // ---------------------------------------------------------------------------------------

    /// Teach the adapter that a chain Hyperion names one way is the chain Axelar names another.
    ///
    /// The two names agree often enough to be tempting and differ often enough to be a bug, so
    /// both are written down and the pair is checked against Axelar itself before it is saved.
    pub fn link_chain(
        env: Env,
        chain: String,
        axelar_chain: String,
        peer: BytesN<20>,
    ) -> Result<(), HyperionError> {
        let cfg = require_admin(&env)?;
        if chain.is_empty() || axelar_chain.is_empty() {
            return Err(HyperionError::UnknownChain);
        }
        // Axelar's hub will not route to a chain it does not trust, so a lane pointing at one
        // is a lane whose first transfer would fail. Better to find out while an admin is
        // watching.
        if !InterchainTokenServiceClient::new(&env, &cfg.its).is_trusted_chain(&axelar_chain) {
            return Err(HyperionError::UnknownChain);
        }
        // The shape that carries a note has to know who is allowed to send it one, and that is
        // the same address it addresses its own transfers to. The bare shape never reads the
        // peer, so it does not have to have one.
        if cfg.route == RouteKind::AxelarGmp && peer == BytesN::from_array(&env, &[0u8; 20]) {
            return Err(HyperionError::ZeroAddressKey);
        }
        if storage::names_retired(&env, &chain, &axelar_chain) {
            return Err(HyperionError::AlreadyConfigured);
        }
        if let Some(existing) = storage::link(&env, &chain) {
            // Identical deployment reruns are harmless; every trust change uses the delayed path.
            if existing.axelar_chain != axelar_chain || existing.peer != peer {
                return Err(HyperionError::AlreadyConfigured);
            }
        }
        if let Some(existing_chain) = storage::chain_of(&env, &axelar_chain) {
            if existing_chain != chain {
                return Err(HyperionError::AlreadyConfigured);
            }
        }
        let link = ChainLink {
            chain,
            axelar_chain,
            peer,
        };
        storage::set_link(&env, &link);
        events::LinkSet {
            chain: link.chain,
            axelar_chain: link.axelar_chain,
            peer: link.peer,
        }
        .publish(&env);
        Ok(())
    }

    /// Propose a rotation of the trusted peer contract on a remote chain.
    ///
    /// Requires an explicit timelock delay of at least [`MIN_PEER_ROTATION_DELAY`].
    pub fn propose_peer_rotation(
        env: Env,
        chain: String,
        new_peer: BytesN<20>,
        delay: u64,
    ) -> Result<(), HyperionError> {
        let cfg = require_admin(&env)?;
        let link = storage::link(&env, &chain).ok_or(HyperionError::UnknownChain)?;
        if cfg.route == RouteKind::AxelarGmp && new_peer == BytesN::from_array(&env, &[0u8; 20]) {
            return Err(HyperionError::ZeroAddressKey);
        }
        if new_peer == link.peer {
            return Err(HyperionError::AlreadyConfigured);
        }
        if delay < MIN_PEER_ROTATION_DELAY {
            return Err(HyperionError::TimelockDelayOutOfRange);
        }
        if storage::pending_rotation(&env, &chain).is_some() {
            return Err(HyperionError::AlreadyConfigured);
        }
        let now = env.ledger().timestamp();
        let eta = now
            .checked_add(delay)
            .ok_or(HyperionError::DecimalOverflow)?;
        storage::set_pending_rotation(
            &env,
            &chain,
            &PendingRotation {
                new_peer: new_peer.clone(),
                eta,
                queued_at: now,
            },
        );
        events::PeerRotationQueued {
            chain,
            current_peer: link.peer,
            new_peer,
            eta,
        }
        .publish(&env);
        Ok(())
    }

    /// Apply a previously queued peer rotation after the timelock delay has elapsed.
    pub fn apply_peer_rotation(env: Env, chain: String) -> Result<(), HyperionError> {
        require_admin(&env)?;
        let mut link = storage::link(&env, &chain).ok_or(HyperionError::UnknownChain)?;
        let pending =
            storage::pending_rotation(&env, &chain).ok_or(HyperionError::TimelockNotQueued)?;
        let now = env.ledger().timestamp();
        if now < pending.eta {
            return Err(HyperionError::TimelockNotReady);
        }
        let old_peer = link.peer.clone();
        link.peer = pending.new_peer.clone();
        storage::set_link(&env, &link);
        storage::clear_pending_rotation(&env, &chain);
        events::PeerRotated {
            chain,
            old_peer,
            new_peer: link.peer,
        }
        .publish(&env);
        Ok(())
    }

    /// Cancel a pending peer rotation.
    pub fn cancel_peer_rotation(env: Env, chain: String) -> Result<(), HyperionError> {
        require_admin(&env)?;
        let _link = storage::link(&env, &chain).ok_or(HyperionError::UnknownChain)?;
        let pending =
            storage::pending_rotation(&env, &chain).ok_or(HyperionError::TimelockNotQueued)?;
        storage::clear_pending_rotation(&env, &chain);
        events::PeerRotationCancelled {
            chain,
            cancelled_peer: pending.new_peer,
        }
        .publish(&env);
        Ok(())
    }

    /// Emergency disable or re-enable a connected chain.
    ///
    /// Takes immediate effect: disabled chains refuse both dispatch and inbound delivery.
    pub fn set_chain_enabled(env: Env, chain: String, enabled: bool) -> Result<(), HyperionError> {
        require_admin(&env)?;
        let _link = storage::link(&env, &chain).ok_or(HyperionError::UnknownChain)?;
        storage::set_chain_disabled(&env, &chain, !enabled);
        events::ChainStatusChanged { chain, enabled }.publish(&env);
        Ok(())
    }

    /// Require a minimum note version for a destination.
    ///
    /// Versions above the local registry are accepted as requirements but block dispatch until
    /// this adapter implements a compatible codec; they are never silently sent as v1.
    pub fn set_required_note_version(
        env: Env,
        chain: String,
        version: u32,
    ) -> Result<(), HyperionError> {
        require_admin(&env)?;
        storage::link(&env, &chain).ok_or(HyperionError::UnknownChain)?;
        if version == 0 || version > u8::MAX as u32 {
            return Err(HyperionError::InvalidLimit);
        }
        storage::set_required_note_version(&env, &chain, version);
        events::NoteVersionRequired { chain, version }.publish(&env);
        Ok(())
    }

    /// Permanently retire both chain names and remove the active forward/reverse mapping.
    pub fn unlink_chain(env: Env, chain: String) -> Result<(), HyperionError> {
        require_admin(&env)?;
        let _link = storage::link(&env, &chain).ok_or(HyperionError::UnknownChain)?;
        storage::remove_link(&env, &chain);
        events::LinkRemoved { chain }.publish(&env);
        Ok(())
    }

    /// Map a local asset to the Axelar token id that carries it.
    ///
    /// Checked against ITS rather than taken on trust. A token id that resolves to a different
    /// asset, or to nothing at all, is a configuration error, and catching it once here means
    /// it surfaces as a legible Hyperion refusal instead of a panic from two contracts away on
    /// somebody's transfer.
    ///
    /// A mapping that is already in place is never rewritten by this call. Where the stored
    /// mapping disagrees with the arguments the call is refused with `AlreadyConfigured` rather
    /// than quietly overwritten, because the reverse mapping is the inbound allowlist and
    /// repointing it would change where arriving funds are paid. Repeating the same mapping is
    /// accepted and writes nothing, so a deploy script that runs twice is boring. To see exactly
    /// which fields disagree, call `check_token_mapping` first.
    pub fn map_token(env: Env, token: Address, token_id: BytesN<32>) -> Result<(), HyperionError> {
        require_admin(&env)?;
        let report = inspect_mapping(&env, &token, &token_id, None, None)?;

        if let Some(conflict) = report.conflicts.first() {
            return Err(match conflict {
                // A second, different mapping over one that already exists. Refused rather than
                // applied, because either direction repoints where money moves.
                TokenMappingConflict::TokenId { .. } | TokenMappingConflict::LocalToken { .. } => {
                    HyperionError::AlreadyConfigured
                }
                // Everything else is Axelar not knowing this asset, which the caller has to fix
                // before the mapping means anything.
                _ => HyperionError::TokenNotMapped,
            });
        }

        // The mapping already reads back exactly as proposed. Writing it again would only
        // republish an event for a change that did not happen, so this is a successful no-op.
        if report.mapped_token_id.as_ref() == Some(&token_id)
            && report.mapped_token.as_ref() == Some(&token)
        {
            return Ok(());
        }

        storage::set_token(&env, &token, &token_id);
        events::TokenMapped { token, token_id }.publish(&env);
        Ok(())
    }

    /// Admit the exact chain/token pair and ITS manager type reviewed for this adapter's route.
    ///
    /// Admission re-reads both ITS registrations. The record is bound to the current Axelar
    /// chain name and peer, so relinking a chain requires a fresh admission instead of inheriting
    /// trust from the previous destination.
    pub fn set_destination_capability(
        env: Env,
        chain: String,
        token: Address,
        capability: DestinationCapability,
    ) -> Result<(), HyperionError> {
        let cfg = require_admin(&env)?;
        if expected_capability(cfg.route)? != capability {
            return Err(HyperionError::UnsupportedRoute);
        }
        let link = storage::link(&env, &chain).ok_or(HyperionError::UnknownChain)?;
        let token_id = storage::token_id(&env, &token).ok_or(HyperionError::TokenNotMapped)?;
        let its = InterchainTokenServiceClient::new(&env, &cfg.its);
        let registered = its
            .try_registered_token_address(&token_id)
            .map_err(|_| HyperionError::TokenNotMapped)?
            .map_err(|_| HyperionError::TokenNotMapped)?;
        if registered != token {
            return Err(HyperionError::TokenNotMapped);
        }
        let token_manager_type = its.token_manager_type(&token_id);
        let record = DestinationCapabilityRecord::new(
            link.chain.clone(),
            link.axelar_chain,
            link.peer,
            token_id.clone(),
            token_manager_type,
            capability,
        );
        storage::set_destination_capability(&env, &record);
        events::DestinationCapabilitySet {
            chain,
            token_id,
            token_manager_type,
            capability,
        }
        .publish(&env);
        Ok(())
    }

    /// Report what registering `token` under `token_id` would do, without doing any of it.
    ///
    /// Answers the question an operator actually has before an activation: not "is this wrong"
    /// but "which fields disagree, and what does Axelar say they should be". Every conflict is
    /// reported at once, in a fixed order, so a rehearsal and a production run print the same
    /// list. Read only: nothing here writes storage or extends a TTL, so calling it twice leaves
    /// the contract exactly as it found it.
    ///
    /// `expected_decimals` and `expected_manager_type` are optional because an operator checking
    /// only the ids should not have to guess at the other two. Passing them adds those checks;
    /// leaving them out reports what was observed without judging it.
    pub fn check_token_mapping(
        env: Env,
        token: Address,
        token_id: BytesN<32>,
        expected_decimals: Option<u32>,
        expected_manager_type: Option<u32>,
    ) -> Result<TokenMappingReport, HyperionError> {
        inspect_mapping(
            &env,
            &token,
            &token_id,
            expected_decimals,
            expected_manager_type,
        )
    }

    /// Hand the admin role to somebody else.
    pub fn set_admin(env: Env, new_admin: Address) -> Result<(), HyperionError> {
        let mut cfg = require_admin(&env)?;
        let old = cfg.admin.clone();
        cfg.admin = new_admin.clone();
        storage::set_config(&env, &cfg);
        events::AdminChanged {
            old,
            new: new_admin,
        }
        .publish(&env);
        Ok(())
    }

    /// Replace this contract's code.
    ///
    /// Gated on the adapter's own admin, which in production is the router's timelocked
    /// multisig. An adapter is the one place in Hyperion that has to change when a rail
    /// changes, so being able to upgrade it is the difference between shipping a fix and
    /// redeploying the world.
    pub fn upgrade(env: Env, wasm_hash: BytesN<32>) -> Result<(), HyperionError> {
        require_admin(&env)?;
        env.deployer()
            .update_current_contract(soroban_sdk::ContractExecutable::Wasm(wasm_hash));
        Ok(())
    }

    // ---------------------------------------------------------------------------------------
    // Views
    // ---------------------------------------------------------------------------------------

    pub fn get_config(env: Env) -> Result<Config, HyperionError> {
        storage::config(&env)
    }

    pub fn get_link(env: Env, chain: String) -> Result<ChainLink, HyperionError> {
        storage::link(&env, &chain).ok_or(HyperionError::UnknownChain)
    }

    pub fn get_required_note_version(env: Env, chain: String) -> Result<u32, HyperionError> {
        storage::link(&env, &chain).ok_or(HyperionError::UnknownChain)?;
        Ok(storage::required_note_version(&env, &chain))
    }

    pub fn get_pending_rotation(
        env: Env,
        chain: String,
    ) -> Result<Option<PendingRotation>, HyperionError> {
        let _ = storage::link(&env, &chain).ok_or(HyperionError::UnknownChain)?;
        Ok(storage::pending_rotation(&env, &chain))
    }

    pub fn is_chain_enabled(env: Env, chain: String) -> Result<bool, HyperionError> {
        let _ = storage::link(&env, &chain).ok_or(HyperionError::UnknownChain)?;
        Ok(!storage::is_chain_disabled(&env, &chain))
    }

    /// Hyperion's name for the chain Axelar calls this.
    pub fn chain_for(env: Env, axelar_chain: String) -> Result<String, HyperionError> {
        storage::chain_of(&env, &axelar_chain).ok_or(HyperionError::UnknownChain)
    }

    pub fn token_id_for(env: Env, token: Address) -> Result<BytesN<32>, HyperionError> {
        storage::token_id(&env, &token).ok_or(HyperionError::TokenNotMapped)
    }

    pub fn token_for(env: Env, token_id: BytesN<32>) -> Result<Address, HyperionError> {
        storage::token_of(&env, &token_id).ok_or(HyperionError::TokenNotMapped)
    }

    pub fn get_destination_capability(
        env: Env,
        chain: String,
        token: Address,
    ) -> Result<DestinationCapabilityRecord, HyperionError> {
        let token_id = storage::token_id(&env, &token).ok_or(HyperionError::TokenNotMapped)?;
        storage::destination_capability(&env, &chain, &token_id)
            .ok_or(HyperionError::UnsupportedRoute)
    }

    /// How ITS will take the funds for this asset, which is the one call this adapter
    /// authorises on the way out.
    pub fn manager_type_for(env: Env, token: Address) -> Result<TokenManagerType, HyperionError> {
        let cfg = storage::config(&env)?;
        let token_id = storage::token_id(&env, &token).ok_or(HyperionError::TokenNotMapped)?;
        Ok(InterchainTokenServiceClient::new(&env, &cfg.its).token_manager_type(&token_id))
    }

    /// Axelar's own flow limit on this asset, if its operator set one.
    ///
    /// Hyperion has a flow limit of its own in the router, and the tighter of the two is what
    /// actually binds. Reading Axelar's here lets the route planner say which one a transfer is
    /// about to run into, rather than letting a user find out from a refusal.
    pub fn rail_flow_limit(env: Env, token: Address) -> Result<Option<i128>, HyperionError> {
        let cfg = storage::config(&env)?;
        let token_id = storage::token_id(&env, &token).ok_or(HyperionError::TokenNotMapped)?;
        Ok(InterchainTokenServiceClient::new(&env, &cfg.its).flow_limit(&token_id))
    }

    /// Read-only inspection of an Axelar outbound note payload, decoding recipient and nonce.
    pub fn inspect_outbound_note(env: Env, note: Bytes) -> Result<OutboundNote, HyperionError> {
        OutboundNote::decode(&env, &note)
    }

    /// Read-only inspection of an Axelar inbound note payload, decoding kind, strkey, and nonce.
    pub fn inspect_inbound_note(env: Env, note: Bytes) -> Result<InboundNote, HyperionError> {
        InboundNote::decode(&env, &note)
    }

    /// Read-only inspection of the exact expected spender and gas recipient parameters for a paid dispatch.
    /// Provides an offline-verifiable baseline to inspect simulated authorization trees against before signing.
    pub fn paid_dispatch_auth_spec(
        env: Env,
        token: Address,
        amount: i128,
        destination_chain: String,
        gas_amount: i128,
    ) -> Result<PaidDispatchAuthSpec, HyperionError> {
        let cfg = storage::config(&env)?;
        if cfg.route != RouteKind::AxelarIts {
            return Err(HyperionError::UnsupportedRoute);
        }
        if amount <= 0 || gas_amount <= 0 {
            return Err(HyperionError::InvalidAmount);
        }
        let _link = storage::link(&env, &destination_chain).ok_or(HyperionError::UnknownChain)?;
        let token_id = storage::token_id(&env, &token).ok_or(HyperionError::TokenNotMapped)?;
        let its = InterchainTokenServiceClient::new(&env, &cfg.its);
        let manager_type = its.token_manager_type(&token_id);
        let takes_by_burning = manager_type.takes_by_burning();
        let token_manager = if takes_by_burning {
            token.clone()
        } else {
            its.deployed_token_manager(&token_id)
        };
        let gas_token = its.native_token_address();
        let gas_recipient = cfg.its.clone();

        Ok(PaidDispatchAuthSpec {
            token,
            amount,
            token_manager,
            takes_by_burning,
            gas_token,
            gas_recipient,
            gas_amount,
        })
    }
}

/// Read the config and insist the admin signed for whatever is about to happen.
fn require_admin(env: &Env) -> Result<Config, HyperionError> {
    let cfg = storage::config(env)?;
    cfg.admin.require_auth();
    Ok(cfg)
}

fn expected_capability(route: RouteKind) -> Result<DestinationCapability, HyperionError> {
    match route {
        RouteKind::AxelarIts => Ok(DestinationCapability::DirectItsTransfer),
        RouteKind::AxelarGmp => Ok(DestinationCapability::GmpPayload),
        _ => Err(HyperionError::UnsupportedRoute),
    }
}

fn admitted_destination_capability(
    env: &Env,
    cfg: &Config,
    link: &ChainLink,
    token_id: &BytesN<32>,
) -> Result<TokenManagerType, HyperionError> {
    let record = storage::destination_capability(env, &link.chain, token_id)
        .ok_or(HyperionError::UnsupportedRoute)?;
    if record.chain != link.chain
        || record.axelar_chain != link.axelar_chain
        || record.peer != link.peer
        || record.token_id != token_id.clone()
        || record.capability != expected_capability(cfg.route)?
    {
        return Err(HyperionError::UnsupportedRoute);
    }
    Ok(record.token_manager_type)
}

fn require_destination_capability(
    env: &Env,
    cfg: &Config,
    link: &ChainLink,
    token_id: &BytesN<32>,
) -> Result<TokenManagerType, HyperionError> {
    let admitted = admitted_destination_capability(env, cfg, link, token_id)?;
    let current = InterchainTokenServiceClient::new(env, &cfg.its).token_manager_type(token_id);
    if admitted != current {
        return Err(HyperionError::UnsupportedRoute);
    }
    Ok(current)
}

/// Observe everything a registration decision depends on, then classify the disagreements.
///
/// Two reads from this contract's own storage, three from ITS, and one from the asset. Every one
/// of them is tolerant: a token id ITS has never heard of, an asset that does not answer
/// `decimals`, and a manager that was never deployed are all facts worth reporting rather than
/// reasons to stop, and turning any of them into an early return would hide the rest of the
/// picture behind the first thing that was wrong.
///
/// Nothing here writes, and the storage reads deliberately skip the TTL bump the transfer paths
/// apply, so a diagnostic run is a no-op on chain state.
fn inspect_mapping(
    env: &Env,
    token: &Address,
    token_id: &BytesN<32>,
    expected_decimals: Option<u32>,
    expected_manager_type: Option<u32>,
) -> Result<TokenMappingReport, HyperionError> {
    let cfg = storage::config(env)?;
    let its = InterchainTokenServiceClient::new(env, &cfg.its);
    let mut report = TokenMappingReport::new(env, token.clone(), token_id.clone());
    report.expected_decimals = expected_decimals;
    report.expected_manager_type = expected_manager_type;

    report.mapped_token_id = storage::peek_token_id(env, token);
    report.mapped_token = storage::peek_token_of(env, token_id);

    // What Axelar itself says is behind the id. Both reads are allowed to fail: the first panics
    // when no manager is deployed, the second when nothing is registered, and either outcome is
    // a fact the report carries rather than an error it returns.
    if let Ok(Ok(registered)) = its.try_registered_token_address(token_id) {
        report.rail_token = Some(registered);
    }
    if let Ok(Ok(manager)) = its.try_deployed_token_manager(token_id) {
        report.rail_manager = Some(manager);
    }
    if let Ok(Ok(kind)) = its.try_token_manager_type(token_id) {
        report.rail_manager_type = Some(kind as u32);
    }

    // SEP-41 metadata, read the same way `check_token_behavior` reads it on the router. A token
    // that cannot answer is not an error here, it is a token whose decimals are unknown.
    let asset = token::Client::new(env, token);
    if let Ok(Ok(decimals)) = asset.try_decimals() {
        report.onchain_decimals = Some(decimals);
    }

    report.classify(env);
    Ok(report)
}

/// The key the router keys replay protection on.
///
/// Axelar identifies a message by a chain name and a message id, and neither is unique on its
/// own. Hashing the pair as XDR rather than as concatenated text matters: XDR carries a length
/// in front of each string, so `("eth", "ereum1")` and `("ethereum", "1")` hash differently,
/// which is exactly the collision a naive join would hand an attacker.
fn replay_key(env: &Env, source_chain: &String, message_id: &String) -> BytesN<32> {
    env.crypto()
        .keccak256(&(source_chain.clone(), message_id.clone()).to_xdr(env))
        .into()
}

fn validate_dispatch(
    env: &Env,
    caller: &Address,
    token: &Address,
    amount: i128,
    chain: &String,
    destination: &BytesN<32>,
    gas_amount: Option<i128>,
) -> Result<(Config, ChainLink, BytesN<32>, BytesN<20>), HyperionError> {
    let cfg = storage::config(env)?;
    if caller != &cfg.router {
        return Err(HyperionError::Unauthorized);
    }
    if gas_amount.is_some() && cfg.route != RouteKind::AxelarIts {
        return Err(HyperionError::UnsupportedRoute);
    }
    if amount <= 0 || gas_amount.is_some_and(|gas| gas <= 0) {
        return Err(HyperionError::InvalidAmount);
    }
    let link = storage::link(env, chain).ok_or(HyperionError::UnknownChain)?;
    if storage::is_chain_disabled(env, chain) {
        return Err(HyperionError::RouteDisabled);
    }
    let token_id = storage::token_id(env, token).ok_or(HyperionError::TokenNotMapped)?;
    require_destination_capability(env, &cfg, &link, &token_id)?;
    let recipient = bytes32_to_evm(env, destination)?;
    // ITS debits both legs from the same balance when gas uses the bridged asset.
    if let Some(gas) = gas_amount {
        if InterchainTokenServiceClient::new(env, &cfg.its).native_token_address() == *token {
            amount
                .checked_add(gas)
                .ok_or(HyperionError::DecimalOverflow)?;
        }
    }
    Ok((cfg, link, token_id, recipient))
}
