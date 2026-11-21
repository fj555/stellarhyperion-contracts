//! Allbridge Core, as this adapter needs to see it.
//!
//! Transcribed from `allbridge-io/allbridge-core-soroban-contracts` rather than imported. The
//! upstream crates are not published, the WASM is pulled in over there with `contractimport!`
//! from a build directory, and vendoring either would drag a second workspace into this one for
//! the sake of six function signatures.
//!
//! What matters is that the shapes below match the wire exactly. Argument order and type are the
//! whole contract, and the tests in this crate run against a stand-in built from the same source.
//!
//! The views at the bottom of this file are how a token is admitted to a lane. They ask Allbridge
//! for its own record of the pool, the pool's token and decimals, the conversion factors derived
//! from those decimals when the pool was added, and the destination bridge's accepted tokens.
//! None of those are Hyperion opinions: a disagreement between them is a rail-side mistake that
//! has to be caught while an admin is watching rather than on the first user transfer.

// The argument count in `swap_and_bridge` is Allbridge's, not a choice made here.
// Transcribing their signature faithfully is the whole point of this file, so the lint is
// switched off rather than the shape reworked into something that would not compile against
// the real contract.
#![allow(clippy::too_many_arguments)]

use hyperion_core::{address::bytes32_to_evm, codec, HyperionError};
use soroban_sdk::{contractclient, contracttype, token, Address, BytesN, Env, Map, U256};

/// Allbridge's own numeric id for the Stellar network.
///
/// The bridge refuses a destination equal to its own id, so this is also the one chain id a
/// Hyperion lane can never point at.
pub const STELLAR_CHAIN_ID: u32 = 7;

/// Allbridge's internal vUSD precision. `add_pool` derives its conversion factors from
/// `10^(ORACLE_PRECISION - decimals)`, so a token above this precision was never registered
/// correctly and cannot be admitted.
pub const ORACLE_PRECISION: u32 = 18;

/// The extra factor Allbridge applies when converting the bridging fee into the token's units.
pub const CHAIN_PRECISION: u32 = 7;

/// The bridge's instance config, as `get_config` returns it.
///
/// Only `messenger` and `can_swap` are read here. The rest is carried so the type decodes.
#[soroban_sdk::contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BridgeConfig {
    pub messenger: Address,
    pub rebalancer: Address,
    pub from_gas_oracle_factor: Map<Address, u128>,
    pub bridging_fee_conversion_factor: Map<Address, u128>,
    pub pools: Map<BytesN<32>, Address>,
    pub can_swap: bool,
}

/// A bridge Allbridge has registered on another chain, with the tokens it will accept.
#[soroban_sdk::contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnotherBridge {
    pub address: BytesN<32>,
    pub tokens: Map<BytesN<32>, bool>,
}

/// What the pool reports about itself.
///
/// Every field is carried because Soroban decodes the whole `contracttype`; the ones this adapter
/// actually checks are `token` and `decimals`. The layout is upstream's `Pool` storage record.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PoolState {
    pub a: u128,
    pub token: Address,
    pub fee_share_bp: u128,
    pub balance_ratio_min_bp: u128,
    pub d: u128,
    pub token_balance: u128,
    pub v_usd_balance: u128,
    pub reserves: u128,
    pub decimals: u32,
    pub total_lp_amount: u128,
    pub admin_fee_share_bp: u128,
    pub acc_reward_per_share_p: u128,
    pub admin_fee_amount: u128,
    pub can_deposit: bool,
    pub can_withdraw: bool,
}

/// The result of admitting a local asset to a lane. Read-only; holding one changes no storage.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetAdmission {
    /// The pool Allbridge currently names for the local token, checked against the pool's own
    /// view of its bridge and token.
    pub pool: Address,
    /// The token's on-chain precision, cross-checked against the pool and Allbridge's factors.
    pub decimals: u32,
    pub allbridge_chain_id: u32,
    /// The destination token the far bridge has accepted, as an EVM word.
    pub receive_token: BytesN<32>,
}

/// The two rail-side facts checked before any asset is used on a lane.
pub struct PoolAdmission {
    pub pool: Address,
    pub decimals: u32,
}

/// The Allbridge Core bridge contract.
#[contractclient(name = "AllbridgeBridgeClient")]
pub trait AllbridgeBridge {
    /// Take `amount` of `token` from `sender` and send it to `recipient` on another chain.
    ///
    /// The two payment arguments are alternatives. `gas_amount` is native token pulled from
    /// `sender`; `fee_token_amount` is carved out of `amount` and converted at the gas oracle's
    /// price. Between them they have to cover the bridge's cost plus the messenger's, or the
    /// call reverts. Hyperion pays in native and leaves `fee_token_amount` at zero, so the
    /// amount that reaches the pool is the amount the router handed over.
    fn swap_and_bridge(
        env: Env,
        sender: Address,
        token: Address,
        amount: u128,
        recipient: BytesN<32>,
        destination_chain_id: u32,
        receive_token: BytesN<32>,
        nonce: U256,
        gas_amount: u128,
        fee_token_amount: u128,
    );

    /// The pool holding a token, keyed by the token's address as thirty two bytes.
    fn get_pool_address(env: Env, token_address: BytesN<32>) -> Address;

    /// What the bridge itself will spend relaying to a chain, in native token.
    ///
    /// Not the whole cost. The messenger charges separately and the bridge checks the sum.
    fn get_transaction_cost(env: Env, chain_id: u32) -> u128;

    fn get_config(env: Env) -> BridgeConfig;

    fn get_another_bridge(env: Env, chain_id: u32) -> AnotherBridge;
}

/// The messenger contract, which carries the attestation and charges for it separately.
#[contractclient(name = "AllbridgeMessengerClient")]
pub trait AllbridgeMessenger {
    fn get_transaction_cost(env: Env, chain_id: u32) -> u128;
}

/// The pool a token lives in. `swap_to_v_usd` is named in the outbound authorisation; the two
/// views are what admission checks before an asset is allowed onto a lane.
#[contractclient(name = "AllbridgePoolClient")]
pub trait AllbridgePool {
    fn swap_to_v_usd(env: Env, user: Address, amount: u128, zero_fee: bool) -> u128;

    /// The pool's own record, carrying the asset it sells and its precision.
    fn get_pool(env: Env) -> PoolState;

    /// The bridge allowed to drive this pool. A pool registered on a different bridge is not the
    /// pool this adapter was admitted against.
    fn get_bridge(env: Env) -> Address;
}

/// Check the rail's own record of a local token and its pool.
///
/// Three independent statements have to agree: the bridge's `pools` entry, the address returned by
/// `get_pool_address`, and the pool's own bridge and token. The token's precision then has to
/// agree with the pool and with both conversion factors Allbridge computed when it added the
/// pool. A disagreement in any of them is refused before an amount is authorised or sold.
pub fn validate_pool(
    env: &Env,
    bridge: &Address,
    rail: &BridgeConfig,
    token: &Address,
) -> Result<PoolAdmission, HyperionError> {
    let token_key = BytesN::from_array(env, &codec::contract_key(env, token)?);
    let pool = rail
        .pools
        .get(token_key.clone())
        .ok_or(HyperionError::TokenNotMapped)?;
    if pool != AllbridgeBridgeClient::new(env, bridge).get_pool_address(&token_key) {
        return Err(HyperionError::TokenNotMapped);
    }

    let pool_client = AllbridgePoolClient::new(env, &pool);
    let state = pool_client.get_pool();
    if pool_client.get_bridge() != *bridge || state.token != *token {
        return Err(HyperionError::TokenNotMapped);
    }

    let decimals = match token::Client::new(env, token).try_decimals() {
        Ok(Ok(value)) => value,
        _ => return Err(HyperionError::InvalidDecimals),
    };
    if decimals > ORACLE_PRECISION || state.decimals != decimals {
        return Err(HyperionError::InvalidDecimals);
    }
    let oracle_factor = 10u128
        .checked_pow(ORACLE_PRECISION - decimals)
        .ok_or(HyperionError::InvalidDecimals)?;
    let fee_factor = 10u128
        .checked_pow(ORACLE_PRECISION + CHAIN_PRECISION - decimals)
        .ok_or(HyperionError::InvalidDecimals)?;
    if rail.from_gas_oracle_factor.get(token.clone()) != Some(oracle_factor)
        || rail.bridging_fee_conversion_factor.get(token.clone()) != Some(fee_factor)
    {
        return Err(HyperionError::InvalidDecimals);
    }

    Ok(PoolAdmission { pool, decimals })
}

/// Check the far side of a lane before a local asset is allowed to use it.
///
/// A receive token is an EVM word left padded with zeroes. Allbridge would carry arbitrary bytes
/// without complaint, so the same word check the destination uses is applied here to the mapping
/// itself, followed by the far bridge's own list of accepted tokens.
pub fn validate_destination(
    env: &Env,
    bridge: &Address,
    allbridge_chain_id: u32,
    receive_token: &BytesN<32>,
) -> Result<(), HyperionError> {
    let _ = bytes32_to_evm(env, receive_token)?;
    let remote = AllbridgeBridgeClient::new(env, bridge)
        .try_get_another_bridge(&allbridge_chain_id)
        .map_err(|_| HyperionError::UnknownChain)?
        .map_err(|_| HyperionError::UnknownChain)?;
    if remote.tokens.get(receive_token.clone()) != Some(true) {
        return Err(HyperionError::TokenNotMapped);
    }
    Ok(())
}

/// Both halves of admission in one call, used when a mapping is written and before it is spent.
pub fn validate_asset_admission(
    env: &Env,
    bridge: &Address,
    rail: &BridgeConfig,
    token: &Address,
    allbridge_chain_id: u32,
    receive_token: &BytesN<32>,
) -> Result<AssetAdmission, HyperionError> {
    let pool = validate_pool(env, bridge, rail, token)?;
    validate_destination(env, bridge, allbridge_chain_id, receive_token)?;
    Ok(AssetAdmission {
        pool: pool.pool,
        decimals: pool.decimals,
        allbridge_chain_id,
        receive_token: receive_token.clone(),
    })
}

/// Allbridge's message digest, byte for byte.
///
/// The two overwritten bytes at the front are not a quirk to be tidied away. The messenger reads
/// the source and destination chain out of them, so a digest built without them routes nowhere.
pub fn hash_message(
    env: &Env,
    amount: u128,
    recipient: &BytesN<32>,
    source_chain_id: u32,
    destination_chain_id: u32,
    receive_token: &BytesN<32>,
    nonce: &U256,
) -> BytesN<32> {
    let mut buf = soroban_sdk::Bytes::new(env);
    buf.extend_from_slice(&[0u8; 16]);
    buf.extend_from_slice(&amount.to_be_bytes());
    buf.extend_from_array(&recipient.to_array());
    buf.append(&U256::from_u32(env, source_chain_id).to_be_bytes());
    buf.extend_from_array(&receive_token.to_array());
    buf.append(&nonce.to_be_bytes());
    buf.extend_from_slice(&1u8.to_be_bytes());

    let digest: BytesN<32> = env.crypto().keccak256(&buf).into();
    let mut out = digest.to_array();
    out[0] = source_chain_id as u8;
    out[1] = destination_chain_id as u8;
    BytesN::from_array(env, &out)
}
