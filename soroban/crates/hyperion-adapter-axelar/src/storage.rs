use hyperion_core::{axelar::DestinationCapabilityRecord, HyperionError};
use soroban_sdk::{contracttype, Address, BytesN, Env, String};

use crate::types::{ChainLink, Config, PendingRotation};

pub const BUMP_THRESHOLD: u32 = 17_280;
pub const BUMP_TO: u32 = 518_400;

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Config,
    /// Hyperion's chain name to the whole lane, for the outbound leg.
    Link(String),
    /// Axelar's chain name back to Hyperion's, for the inbound leg. Two keys rather than one
    /// scan, because a contract cannot iterate storage and both directions are on a hot path.
    Source(String),
    /// Local asset to the Axelar token id that carries it.
    TokenId(Address),
    /// And back again.
    ///
    /// This doubles as the inbound allowlist. ITS will happily deliver any token id it knows
    /// about; an unmapped one is refused here, which is the correct behaviour on the day Axelar
    /// registers a token nobody on this side has looked at yet.
    Token(BytesN<32>),
    /// Pending peer rotation for a chain.
    PendingRotation(String),
    /// Minimum note version required by the configured destination. Missing means v1 for
    /// installations predating version negotiation.
    RequiredNoteVersion(String),
    DestinationCapability(String, BytesN<32>),
    /// Emergency disable flag for a chain.
    Disabled(String),
    /// Retired names cannot be reused to bypass a peer rotation delay.
    RetiredChain(String),
    RetiredSource(String),
}

pub fn config(env: &Env) -> Result<Config, HyperionError> {
    env.storage()
        .instance()
        .get(&DataKey::Config)
        .ok_or(HyperionError::NotInitialized)
}

pub fn set_config(env: &Env, cfg: &Config) {
    env.storage().instance().set(&DataKey::Config, cfg);
    env.storage().instance().extend_ttl(BUMP_THRESHOLD, BUMP_TO);
}

pub fn is_initialized(env: &Env) -> bool {
    env.storage().instance().has(&DataKey::Config)
}

pub fn link(env: &Env, chain: &String) -> Option<ChainLink> {
    let key = DataKey::Link(chain.clone());
    let found: Option<ChainLink> = env.storage().persistent().get(&key);
    if found.is_some() {
        env.storage()
            .persistent()
            .extend_ttl(&key, BUMP_THRESHOLD, BUMP_TO);
    }
    found
}

pub fn chain_of(env: &Env, axelar_chain: &String) -> Option<String> {
    let key = DataKey::Source(axelar_chain.clone());
    let found: Option<String> = env.storage().persistent().get(&key);
    if found.is_some() {
        env.storage()
            .persistent()
            .extend_ttl(&key, BUMP_THRESHOLD, BUMP_TO);
    }
    found
}

pub fn set_link(env: &Env, link: &ChainLink) {
    let link_key = DataKey::Link(link.chain.clone());
    let source_key = DataKey::Source(link.axelar_chain.clone());
    env.storage().persistent().set(&link_key, link);
    env.storage().persistent().set(&source_key, &link.chain);
    env.storage()
        .persistent()
        .extend_ttl(&link_key, BUMP_THRESHOLD, BUMP_TO);
    env.storage()
        .persistent()
        .extend_ttl(&source_key, BUMP_THRESHOLD, BUMP_TO);
}

pub fn token_id(env: &Env, token: &Address) -> Option<BytesN<32>> {
    let key = DataKey::TokenId(token.clone());
    let found: Option<BytesN<32>> = env.storage().persistent().get(&key);
    if found.is_some() {
        env.storage()
            .persistent()
            .extend_ttl(&key, BUMP_THRESHOLD, BUMP_TO);
    }
    found
}

pub fn token_of(env: &Env, token_id: &BytesN<32>) -> Option<Address> {
    let key = DataKey::Token(token_id.clone());
    let found: Option<Address> = env.storage().persistent().get(&key);
    if found.is_some() {
        env.storage()
            .persistent()
            .extend_ttl(&key, BUMP_THRESHOLD, BUMP_TO);
    }
    found
}

/// The same two reads without the TTL bump.
///
/// `token_id` and `token_of` extend a live entry's lifetime as a side effect, which is right on
/// the transfer paths and wrong for a check whose contract is that validating a mapping changes
/// nothing. A dry run that quietly renewed storage would make "no side effects" a claim nobody
/// could test, so the diagnostic reads go through here instead.
pub fn peek_token_id(env: &Env, token: &Address) -> Option<BytesN<32>> {
    env.storage()
        .persistent()
        .get(&DataKey::TokenId(token.clone()))
}

pub fn peek_token_of(env: &Env, token_id: &BytesN<32>) -> Option<Address> {
    env.storage()
        .persistent()
        .get(&DataKey::Token(token_id.clone()))
}

pub fn set_token(env: &Env, token: &Address, token_id: &BytesN<32>) {
    let id_key = DataKey::TokenId(token.clone());
    let tok_key = DataKey::Token(token_id.clone());
    env.storage().persistent().set(&id_key, token_id);
    env.storage().persistent().set(&tok_key, token);
    env.storage()
        .persistent()
        .extend_ttl(&id_key, BUMP_THRESHOLD, BUMP_TO);
    env.storage()
        .persistent()
        .extend_ttl(&tok_key, BUMP_THRESHOLD, BUMP_TO);
}

pub fn destination_capability(
    env: &Env,
    chain: &String,
    token_id: &BytesN<32>,
) -> Option<DestinationCapabilityRecord> {
    let key = DataKey::DestinationCapability(chain.clone(), token_id.clone());
    env.storage().instance().get(&key)
}

pub fn set_destination_capability(env: &Env, record: &DestinationCapabilityRecord) {
    let key = DataKey::DestinationCapability(record.chain.clone(), record.token_id.clone());
    // Instance storage shares the configuration's TTL, which is already extended by bounded
    // maintenance. Keeping admission with the instance prevents it expiring independently and
    // silently disabling a still-active chain/token pair.
    env.storage().instance().set(&key, record);
    env.storage().instance().extend_ttl(BUMP_THRESHOLD, BUMP_TO);
}

/// Top a lane's lifetime back up without changing it.
///
/// Soroban archives storage nobody reads, and a lane that stays quiet for a month is exactly the
/// lane somebody will try to use on the day it has expired. The read paths above bump as they go,
/// so this only matters for a lane with no traffic at all, which is why it is permissionless.
pub fn touch_link(env: &Env, chain: &String) -> u32 {
    let key = DataKey::Link(chain.clone());
    let Some(found) = env.storage().persistent().get::<_, ChainLink>(&key) else {
        return 0;
    };
    env.storage()
        .persistent()
        .extend_ttl(&key, BUMP_THRESHOLD, BUMP_TO);
    let mut touched = 1u32;
    let source = DataKey::Source(found.axelar_chain);
    if env.storage().persistent().has(&source) {
        env.storage()
            .persistent()
            .extend_ttl(&source, BUMP_THRESHOLD, BUMP_TO);
        touched += 1;
    }
    touched
}

/// New rotation/disable metadata shares the instance lifetime with configuration.
pub fn pending_rotation(env: &Env, chain: &String) -> Option<PendingRotation> {
    env.storage()
        .instance()
        .get(&DataKey::PendingRotation(chain.clone()))
}

pub fn set_pending_rotation(env: &Env, chain: &String, rotation: &PendingRotation) {
    env.storage()
        .instance()
        .set(&DataKey::PendingRotation(chain.clone()), rotation);
    env.storage().instance().extend_ttl(BUMP_THRESHOLD, BUMP_TO);
}

pub fn clear_pending_rotation(env: &Env, chain: &String) {
    env.storage()
        .instance()
        .remove(&DataKey::PendingRotation(chain.clone()));
}

pub fn is_chain_disabled(env: &Env, chain: &String) -> bool {
    env.storage()
        .instance()
        .get(&DataKey::Disabled(chain.clone()))
        .unwrap_or(false)
}

pub fn set_chain_disabled(env: &Env, chain: &String, disabled: bool) {
    let key = DataKey::Disabled(chain.clone());
    if disabled {
        env.storage().instance().set(&key, &true);
    } else {
        env.storage().instance().remove(&key);
    }
    env.storage().instance().extend_ttl(BUMP_THRESHOLD, BUMP_TO);
}

pub fn names_retired(env: &Env, chain: &String, axelar_chain: &String) -> bool {
    env.storage()
        .instance()
        .has(&DataKey::RetiredChain(chain.clone()))
        || env
            .storage()
            .instance()
            .has(&DataKey::RetiredSource(axelar_chain.clone()))
}

pub fn remove_link(env: &Env, chain: &String) {
    if let Some(link) = link(env, chain) {
        env.storage()
            .instance()
            .set(&DataKey::RetiredChain(chain.clone()), &true);
        env.storage()
            .instance()
            .set(&DataKey::RetiredSource(link.axelar_chain.clone()), &true);
        env.storage().instance().extend_ttl(BUMP_THRESHOLD, BUMP_TO);
        env.storage()
            .persistent()
            .remove(&DataKey::Source(link.axelar_chain));
    }
    env.storage()
        .persistent()
        .remove(&DataKey::Link(chain.clone()));
    clear_pending_rotation(env, chain);
    env.storage()
        .instance()
        .remove(&DataKey::RequiredNoteVersion(chain.clone()));
    set_chain_disabled(env, chain, false);
}

pub fn required_note_version(env: &Env, chain: &String) -> u32 {
    env.storage()
        .instance()
        .get(&DataKey::RequiredNoteVersion(chain.clone()))
        .unwrap_or(u32::from(hyperion_core::axelar::NOTE_VERSION_V1))
}

pub fn set_required_note_version(env: &Env, chain: &String, version: u32) {
    env.storage()
        .instance()
        .set(&DataKey::RequiredNoteVersion(chain.clone()), &version);
    env.storage().instance().extend_ttl(BUMP_THRESHOLD, BUMP_TO);
}

pub fn touch_token(env: &Env, token: &Address) -> u32 {
    let key = DataKey::TokenId(token.clone());
    let Some(id) = env.storage().persistent().get::<_, BytesN<32>>(&key) else {
        return 0;
    };
    env.storage()
        .persistent()
        .extend_ttl(&key, BUMP_THRESHOLD, BUMP_TO);
    let mut touched = 1u32;
    let rev = DataKey::Token(id);
    if env.storage().persistent().has(&rev) {
        env.storage()
            .persistent()
            .extend_ttl(&rev, BUMP_THRESHOLD, BUMP_TO);
        touched += 1;
    }
    touched
}

pub fn touch_remote_token(env: &Env, token_id: &BytesN<32>) -> u32 {
    let key = DataKey::Token(token_id.clone());
    let Some(token) = env.storage().persistent().get::<_, Address>(&key) else {
        return 0;
    };
    env.storage()
        .persistent()
        .extend_ttl(&key, BUMP_THRESHOLD, BUMP_TO);
    let mut touched = 1u32;
    let fwd = DataKey::TokenId(token);
    if env.storage().persistent().has(&fwd) {
        env.storage()
            .persistent()
            .extend_ttl(&fwd, BUMP_THRESHOLD, BUMP_TO);
        touched += 1;
    }
    touched
}
