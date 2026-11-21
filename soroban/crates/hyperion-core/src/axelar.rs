//! Hyperion's own note, carried in the payload slot the Axelar rails leave open.
//!
//! ITS gives every transfer an optional `data` field. When it is set, the tokens land on the
//! contract named as the destination and that contract gets called back with the data. That is
//! the whole of what Hyperion needs from it: somewhere to say who the money is actually for,
//! because the address ITS delivers to is Hyperion's own contract rather than the person who
//! asked for the transfer.
//!
//! Two shapes, one per direction, because the two directions describe different things. Leaving
//! Stellar the note names an EVM address, which is twenty flat bytes the far side reads with a
//! slice. Arriving on Stellar it names a Stellar destination, and there the note carries a
//! strkey rather than a raw key, for the reason set out in [`crate::strkey`]: the checksum comes
//! free with the format and it catches a flipped bit before anybody gets paid.
//!
//! Both carry the originating router's nonce. No contract needs it. The person watching a
//! transfer in the app does, because it is the one value that appears on both sides of the hop,
//! and it lets an indexer say *this delivery is that transfer* rather than guessing from amounts
//! and timestamps.
//!
//! The other half of what Hyperion and Axelar have to agree on lives here too: the vocabulary for
//! describing why a proposed token registration is incompatible with the one already on chain.
//! Registration is an admin action taken once per asset, and the failure that matters is a second
//! registration that quietly rewrites the first. [`TokenMappingConflict`] names the exact fields
//! that disagree so the answer is a legible refusal rather than a mapping nobody can explain.

use soroban_sdk::{contracttype, Address, Bytes, BytesN, Env, String, Vec};

#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum TokenManagerType {
    NativeInterchainToken = 0,
    MintBurnFrom = 1,
    LockUnlock = 2,
    MintBurn = 4,
}

impl TokenManagerType {
    // Whether ITS takes funds by burning them out of the sender's balance.
    //
    // Three of the four do. Only `LockUnlock` moves the funds somewhere, which is the one case
    // where the authorisation this adapter grants has to name a third party.
    pub fn takes_by_burning(&self) -> bool {
        !matches!(self, TokenManagerType::LockUnlock)
    }
}

#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DestinationCapability {
    DirectItsTransfer,
    GmpPayload,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DestinationCapabilityRecord {
    pub chain: String,
    pub axelar_chain: String,
    pub peer: BytesN<20>,
    pub token_id: BytesN<32>,
    pub token_manager_type: TokenManagerType,
    pub capability: DestinationCapability,
}

impl DestinationCapabilityRecord {
    pub fn new(
        chain: String,
        axelar_chain: String,
        peer: BytesN<20>,
        token_id: BytesN<32>,
        token_manager_type: TokenManagerType,
        capability: DestinationCapability,
    ) -> Self {
        Self {
            chain,
            axelar_chain,
            peer,
            token_id,
            token_manager_type,
            capability,
        }
    }
}

use crate::error::HyperionError;
use crate::strkey::StrkeyDestination;

/// Version byte leading every Hyperion note on these rails.
///
/// Shared with [`crate::cctp::HOOK_VERSION`] on purpose. Two rails, one envelope version, so a
/// reader never has to remember which rail numbers its payloads differently from the other.
pub const NOTE_VERSION: u8 = 1;
pub const NOTE_VERSION_V1: u8 = NOTE_VERSION;
pub const SUPPORTED_NOTE_VERSIONS: [u8; 1] = [NOTE_VERSION_V1];

/// Whether this build has a decoder and encoder for a note version.
pub fn supports_note_version(version: u8) -> bool {
    matches!(version, NOTE_VERSION_V1)
}

/// Negotiate a version without falling below the destination's declared minimum.
pub fn negotiate_note_version(
    required_version: u8,
    local_versions: &[u8],
    destination_versions: &[u8],
) -> Result<u8, HyperionError> {
    local_versions
        .iter()
        .copied()
        .filter(|version| *version >= required_version && supports_note_version(*version))
        .filter(|version| destination_versions.contains(version))
        .max()
        .ok_or(HyperionError::UnsupportedHookVersion)
}

/// Minimum delay required before a proposed peer rotation can become active.
///
/// 3600 seconds (1 hour) enforces an explicit review and timelock window so that
/// malicious or accidental peer migrations can be detected, audited, and cancelled
/// before becoming effective.
pub const MIN_PEER_ROTATION_DELAY: u64 = 3_600;

/// Version, twenty byte address, eight byte nonce.
pub const OUTBOUND_NOTE_LEN: u32 = 29;

/// The shortest an inbound note can be: version, kind, length, a strkey, a nonce.
const INBOUND_NOTE_MIN: u32 = 1 + 3 + 8;

/// Eight bytes of big-endian nonce sitting at the end of a note.
const NONCE_LEN: u32 = 8;

/// What Hyperion says to its own contract on the far side when a transfer leaves Stellar.
///
/// Wire form is `[version][20 byte recipient][8 byte big-endian nonce]`, packed rather than ABI
/// encoded. The consumer is one Solidity function reading three fixed slices, and a packed
/// layout is both cheaper for it and something this crate can write without an ABI encoder.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutboundNote {
    /// Who the tokens are for once they arrive.
    pub recipient: BytesN<20>,
    /// The Stellar router's outbound sequence number for this transfer.
    pub nonce: u64,
}

impl OutboundNote {
    pub fn new(recipient: BytesN<20>, nonce: u64) -> Self {
        Self { recipient, nonce }
    }

    pub fn encode(&self, env: &Env) -> Result<Bytes, HyperionError> {
        if self.recipient == BytesN::from_array(env, &[0u8; 20]) {
            return Err(HyperionError::ZeroAddressKey);
        }
        let mut out = Bytes::new(env);
        out.push_back(NOTE_VERSION);
        out.append(&Bytes::from_array(env, &self.recipient.to_array()));
        out.append(&Bytes::from_array(env, &self.nonce.to_be_bytes()));
        Ok(out)
    }

    /// Read a note back, which the Stellar side only ever does in its own tests.
    ///
    /// Kept here rather than in the test module because a format with no decoder is a format
    /// nobody can prove round trips, and this one has to agree with a Solidity reader byte for
    /// byte.
    pub fn decode(env: &Env, raw: &Bytes) -> Result<Self, HyperionError> {
        let version = raw.get(0).ok_or(HyperionError::MalformedMessage)?;
        match version {
            NOTE_VERSION_V1 => Self::decode_v1(env, raw),
            _ => Err(HyperionError::UnsupportedHookVersion),
        }
    }

    fn decode_v1(env: &Env, raw: &Bytes) -> Result<Self, HyperionError> {
        if raw.len() != OUTBOUND_NOTE_LEN {
            return Err(HyperionError::MalformedMessage);
        }
        let mut address = [0u8; 20];
        let mut i = 0u32;
        while i < 20 {
            address[i as usize] = raw.get(1 + i).ok_or(HyperionError::MalformedMessage)?;
            i += 1;
        }
        let note = Self {
            recipient: BytesN::from_array(env, &address),
            nonce: read_nonce(raw, 21)?,
        };
        if note.recipient == BytesN::from_array(env, &[0u8; 20]) {
            return Err(HyperionError::ZeroAddressKey);
        }
        Ok(note)
    }
}

/// What Hyperion's contract on the far side says when a transfer arrives on Stellar.
///
/// Wire form is `[version][StrkeyDestination][8 byte big-endian nonce]`, where the middle part
/// is exactly what [`StrkeyDestination::encode`] produces. Variable length, because a muxed
/// strkey is thirteen characters longer than a plain one, and the length byte inside the
/// destination is what says which.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InboundNote {
    /// Where the tokens go once the adapter has handed them to the router.
    pub destination: StrkeyDestination,
    /// The far side's own sequence number for this transfer.
    pub nonce: u64,
}

impl InboundNote {
    pub fn new(destination: StrkeyDestination, nonce: u64) -> Self {
        Self { destination, nonce }
    }

    pub fn encode(&self, env: &Env) -> Result<Bytes, HyperionError> {
        let mut out = Bytes::new(env);
        out.push_back(NOTE_VERSION);
        out.append(&self.destination.encode(env)?);
        out.append(&Bytes::from_array(env, &self.nonce.to_be_bytes()));
        Ok(out)
    }

    /// Parse a note, refusing a version this build does not understand.
    ///
    /// An unknown version is refused rather than skipped past, for the same reason the CCTP hook
    /// refuses one: a payload we cannot read is a destination we would be guessing at, and
    /// guessing here means paying the wrong person.
    pub fn decode(env: &Env, raw: &Bytes) -> Result<Self, HyperionError> {
        let version = raw.get(0).ok_or(HyperionError::MalformedMessage)?;
        match version {
            NOTE_VERSION_V1 => Self::decode_v1(env, raw),
            _ => Err(HyperionError::UnsupportedHookVersion),
        }
    }

    fn decode_v1(env: &Env, raw: &Bytes) -> Result<Self, HyperionError> {
        if raw.len() < INBOUND_NOTE_MIN {
            return Err(HyperionError::MalformedMessage);
        }
        let nonce = read_nonce(raw, raw.len() - NONCE_LEN)?;
        let destination = StrkeyDestination::decode(env, &raw.slice(1..raw.len() - NONCE_LEN))?;
        Ok(Self { destination, nonce })
    }

    /// Whether this destination has to opt into the asset before it can be paid.
    pub fn needs_trustline(&self) -> bool {
        self.destination.kind.needs_trustline()
    }
}

/// Discrepancy identified during Axelar note-to-payment reconciliation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconciliationDiscrepancy {
    /// The recipient identified in the source note disagrees with the candidate payment recipient.
    RecipientMismatch,
    /// The Axelar token ID in the source dispatch disagrees with the candidate payment token ID.
    TokenIdMismatch,
    /// The integer amount dispatched disagrees with the candidate payment amount.
    AmountMismatch,
    /// Full command identity is missing, empty or malformed on the source or candidate receipt.
    CommandIdentityMissing,
    /// The complete source identity differs from the destination receipt identity.
    CommandIdentityMismatch,
    /// Amounts must be positive; matching invalid values do not establish a payment.
    InvalidAmount,
    /// An absent source token binding cannot establish agreement.
    InvalidTokenIdentity,
}

/// Candidate ITS payment receipt for an outbound transfer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CandidatePayment {
    /// Candidate command identity; equality is development evidence, never proof of execution.
    pub command_id: BytesN<32>,
    /// Token ID delivered.
    pub token_id: BytesN<32>,
    /// Exact integer amount delivered.
    pub amount: i128,
    /// Final recipient address on the destination chain.
    pub recipient: BytesN<20>,
}

/// Candidate ITS payment receipt arriving on Stellar.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CandidateInboundPayment {
    /// Axelar command / message identifier.
    pub message_id: String,
    /// Token ID delivered.
    pub token_id: BytesN<32>,
    /// Exact integer amount delivered.
    pub amount: i128,
    /// Final destination strkey on Stellar.
    pub recipient: StrkeyDestination,
}

/// Verify that an outbound note's intent and an ITS destination payment agree exactly.
///
/// Acceptance criteria:
/// - Recipient, token ID, amount and full command identity must all agree.
/// - Returns Err with specific discrepancy if any field differs or command identity is missing.
/// - Does not rely on the display nonce for verification correctness.
///
/// This compares supplied development evidence; it does not authenticate an event, verify finality,
/// authorize a dispatch or establish that the destination executed a payment.
pub fn verify_outbound_reconciliation(
    note: &OutboundNote,
    dispatched_token_id: &BytesN<32>,
    dispatched_amount: i128,
    expected_command_id: &BytesN<32>,
    candidate: &CandidatePayment,
) -> Result<(), ReconciliationDiscrepancy> {
    let empty_id = BytesN::from_array(candidate.token_id.env(), &[0u8; 32]);
    if expected_command_id == &empty_id || candidate.command_id == empty_id {
        return Err(ReconciliationDiscrepancy::CommandIdentityMissing);
    }
    if expected_command_id != &candidate.command_id {
        return Err(ReconciliationDiscrepancy::CommandIdentityMismatch);
    }
    if dispatched_amount <= 0 || candidate.amount <= 0 {
        return Err(ReconciliationDiscrepancy::InvalidAmount);
    }
    if dispatched_token_id == &empty_id {
        return Err(ReconciliationDiscrepancy::InvalidTokenIdentity);
    }
    if note.recipient != candidate.recipient {
        return Err(ReconciliationDiscrepancy::RecipientMismatch);
    }
    if dispatched_token_id != &candidate.token_id {
        return Err(ReconciliationDiscrepancy::TokenIdMismatch);
    }
    if dispatched_amount != candidate.amount {
        return Err(ReconciliationDiscrepancy::AmountMismatch);
    }
    Ok(())
}

/// Verify that an inbound note's intent and an ITS destination payment agree exactly.
pub fn verify_inbound_reconciliation(
    note: &InboundNote,
    dispatched_token_id: &BytesN<32>,
    dispatched_amount: i128,
    expected_message_id: &String,
    candidate: &CandidateInboundPayment,
) -> Result<(), ReconciliationDiscrepancy> {
    if !valid_message_identity(expected_message_id)
        || !valid_message_identity(&candidate.message_id)
    {
        return Err(ReconciliationDiscrepancy::CommandIdentityMissing);
    }
    if expected_message_id != &candidate.message_id {
        return Err(ReconciliationDiscrepancy::CommandIdentityMismatch);
    }
    if dispatched_amount <= 0 || candidate.amount <= 0 {
        return Err(ReconciliationDiscrepancy::InvalidAmount);
    }
    if dispatched_token_id == &BytesN::from_array(dispatched_token_id.env(), &[0; 32]) {
        return Err(ReconciliationDiscrepancy::InvalidTokenIdentity);
    }
    if note.destination != candidate.recipient {
        return Err(ReconciliationDiscrepancy::RecipientMismatch);
    }
    if dispatched_token_id != &candidate.token_id {
        return Err(ReconciliationDiscrepancy::TokenIdMismatch);
    }
    if dispatched_amount != candidate.amount {
        return Err(ReconciliationDiscrepancy::AmountMismatch);
    }
    Ok(())
}

/// Bound opaque message identities consistently with the development CLI and SDK.
fn valid_message_identity(identity: &String) -> bool {
    if identity.is_empty() || identity.len() > 512 {
        return false;
    }
    let bytes = identity.to_bytes();
    (0..bytes.len()).all(|index| matches!(bytes.get(index), Some(0x21..=0x7e)))
}

/// Eight big-endian bytes starting at `offset`.
fn read_nonce(raw: &Bytes, offset: u32) -> Result<u64, HyperionError> {
    let mut value = 0u64;
    let mut i = 0u32;
    while i < NONCE_LEN {
        let byte = raw.get(offset + i).ok_or(HyperionError::MalformedMessage)? as u64;
        value = (value << 8) | byte;
        i += 1;
    }
    Ok(value)
}

/// Authorized debit in a paid dispatch authorization tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizedDebit {
    pub contract: Address,
    pub spender: Address,
    pub amount: i128,
}

/// Authorization snapshot representing the authenticated debits of an Axelar paid dispatch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaidDispatchAuthSnapshot {
    /// The user account authenticating the debits.
    pub sender: Address,
    /// Principal bridge debit (burn or transfer to manager).
    pub principal_token: Address,
    pub principal_spender: Address,
    pub principal_amount: i128,
    pub is_burn: bool,
    /// Fee debit paid to the treasury.
    pub fee_token: Address,
    pub fee_recipient: Address,
    pub fee_amount: i128,
    /// Gas debit paid to Axelar ITS / gas service.
    pub gas_token: Address,
    pub gas_recipient: Address,
    pub gas_amount: i128,
}

/// Potential privilege expansions detected when inspecting an authorization tree against expectations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PrivilegeExpansion {
    SenderMismatch,
    ChangedPrincipalSpender,
    ChangedGasRecipient,
    ChangedFeeRecipient,
    PrincipalAmountMismatch,
    FeeAmountMismatch,
    GasAmountMismatch,
    UnexpectedBurnStatus,
}

/// Verify candidate paid dispatch authorization parameters against expected snapshot baseline.
pub fn inspect_auth_tree_snapshot(
    expected: &PaidDispatchAuthSnapshot,
    candidate: &PaidDispatchAuthSnapshot,
) -> Result<(), PrivilegeExpansion> {
    if expected.sender != candidate.sender {
        return Err(PrivilegeExpansion::SenderMismatch);
    }
    if expected.principal_token != candidate.principal_token {
        return Err(PrivilegeExpansion::ChangedPrincipalSpender);
    }
    if expected.principal_spender != candidate.principal_spender {
        return Err(PrivilegeExpansion::ChangedPrincipalSpender);
    }
    if expected.is_burn != candidate.is_burn {
        return Err(PrivilegeExpansion::UnexpectedBurnStatus);
    }
    if expected.principal_amount != candidate.principal_amount {
        return Err(PrivilegeExpansion::PrincipalAmountMismatch);
    }
    if expected.fee_token != candidate.fee_token {
        return Err(PrivilegeExpansion::ChangedFeeRecipient);
    }
    if expected.fee_recipient != candidate.fee_recipient {
        return Err(PrivilegeExpansion::ChangedFeeRecipient);
    }
    if expected.fee_amount != candidate.fee_amount {
        return Err(PrivilegeExpansion::FeeAmountMismatch);
    }
    if expected.gas_token != candidate.gas_token {
        return Err(PrivilegeExpansion::ChangedGasRecipient);
    }
    if expected.gas_recipient != candidate.gas_recipient {
        return Err(PrivilegeExpansion::ChangedGasRecipient);
    }
    if expected.gas_amount != candidate.gas_amount {
        return Err(PrivilegeExpansion::GasAmountMismatch);
    }
    Ok(())
}

/// One way a proposed token registration disagrees with what is already on chain.
///
/// Each variant names the field, not the symptom. "TokenNotMapped" tells an operator that
/// something is wrong; `TokenId` with the value already stored and the value being proposed tells
/// them which spreadsheet row to fix. The variants are deliberately one per field so a report can
/// list several at once rather than stopping at the first.
///
/// The two `Unregistered*` variants exist because a conflict is not the only way a registration can
/// fail. A token id Axelar has never heard of, or a local asset with no readable metadata, is a
/// different mistake with a different fix, and folding it into "conflict" would send somebody
/// looking for a second mapping that does not exist.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TokenMappingConflict {
    /// The local asset is already mapped to a different token id: (existing, proposed).
    TokenId(BytesN<32>, BytesN<32>),
    /// The token id is already mapped to a different local asset: (existing, proposed). This is
    /// the direction that silently makes an inbound delivery pay the wrong token if it is allowed
    /// to overwrite.
    LocalToken(Address, Address),
    /// The local asset reports different decimals than the registration expects:
    /// (on chain, expected).
    Decimals(u32, u32),
    /// ITS takes funds for this token id through a different kind of token manager than the
    /// registration expects: (on chain, expected). A `LockUnlock` manager holds a balance and
    /// needs an authorisation naming a third party; the burning kinds do not, so getting this
    /// wrong changes what the outbound path authorises as well as what the inbound path trusts.
    ManagerType(u32, u32),
    /// The local asset does not implement SEP-41 metadata this build can read.
    UnreadableMetadata,
    /// Axelar has no token manager, and so no registered asset, behind this token id.
    UnregisteredTokenId,
    /// Axelar's registered asset for this token id is a different contract than the one proposed:
    /// (registered, proposed).
    RailTokenMismatch(Address, Address),
}

/// What a registration check found, whether or not it found a problem.
///
/// Returned rather than only raised so a dry run can print the whole picture: which token id, which
/// local asset, what Axelar says is registered behind it, which manager would move the funds, and
/// every disagreement at once. A view that only threw on the first conflict would need as many calls
/// as there are fields to answer the same question.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TokenMappingReport {
    /// The local asset the caller proposed.
    pub token: Address,
    /// The token id the caller proposed.
    pub token_id: BytesN<32>,
    /// The token id already mapped to `token`, if any.
    pub mapped_token_id: Option<BytesN<32>>,
    /// The local asset already mapped to `token_id`, if any.
    pub mapped_token: Option<Address>,
    /// The asset Axelar's own registry reports for `token_id`, when it knows the id at all.
    pub rail_token: Option<Address>,
    /// The token manager Axelar would move funds through, when one is deployed.
    pub rail_manager: Option<Address>,
    /// How that manager takes the funds, as Axelar's own discriminant.
    pub rail_manager_type: Option<u32>,
    /// Decimals the local asset reports, when it implements readable SEP-41 metadata.
    pub onchain_decimals: Option<u32>,
    /// Decimals the caller said to expect, when they said.
    pub expected_decimals: Option<u32>,
    /// The manager kind the caller said to expect, when they said.
    pub expected_manager_type: Option<u32>,
    /// Every disagreement found, in a stable order. Empty means the mapping is compatible.
    pub conflicts: Vec<TokenMappingConflict>,
}

impl TokenMappingReport {
    /// A report with nothing observed yet and no conflicts.
    pub fn new(env: &Env, token: Address, token_id: BytesN<32>) -> Self {
        Self {
            token,
            token_id,
            mapped_token_id: None,
            mapped_token: None,
            rail_token: None,
            rail_manager: None,
            rail_manager_type: None,
            onchain_decimals: None,
            expected_decimals: None,
            expected_manager_type: None,
            conflicts: Vec::new(env),
        }
    }

    /// Whether this report describes a mapping that can be activated as it stands.
    pub fn is_compatible(&self) -> bool {
        self.conflicts.is_empty()
    }

    /// Derive every disagreement from the observations already recorded on this report.
    ///
    /// Pure, and separated from the reads that populate it, because these are the rules both
    /// chains have to agree on and they are worth testing without a mock rail behind them. The
    /// order is fixed so two runs over the same facts print the same list, which is what makes a
    /// diagnostic worth diffing between a rehearsal and production.
    pub fn classify(&mut self, env: &Env) {
        let mut conflicts = Vec::new(env);

        // A mapping that would move an asset that is already mapped elsewhere is the one that
        // silently changes where money goes. Both directions are checked, because they fail
        // differently: the forward one breaks outbound dispatch, the reverse one makes an inbound
        // delivery pay a token nobody registered.
        if let Some(existing) = &self.mapped_token_id {
            if existing != &self.token_id {
                conflicts.push_back(TokenMappingConflict::TokenId(
                    existing.clone(),
                    self.token_id.clone(),
                ));
            }
        }
        if let Some(existing) = &self.mapped_token {
            if existing != &self.token {
                conflicts.push_back(TokenMappingConflict::LocalToken(
                    existing.clone(),
                    self.token.clone(),
                ));
            }
        }

        // What Axelar itself says is behind this id. Absent means ITS has no manager deployed for
        // it, so there is nothing to route through and nothing to trust on the inbound leg.
        match &self.rail_token {
            None => conflicts.push_back(TokenMappingConflict::UnregisteredTokenId),
            Some(registered) if registered != &self.token => {
                conflicts.push_back(TokenMappingConflict::RailTokenMismatch(
                    registered.clone(),
                    self.token.clone(),
                ));
            }
            Some(_) => {}
        }

        if let Some(expected) = self.expected_decimals {
            match self.onchain_decimals {
                None => conflicts.push_back(TokenMappingConflict::UnreadableMetadata),
                Some(onchain) if onchain != expected => {
                    conflicts.push_back(TokenMappingConflict::Decimals(onchain, expected));
                }
                Some(_) => {}
            }
        }
        if let Some(expected) = self.expected_manager_type {
            match self.rail_manager_type {
                None => conflicts.push_back(TokenMappingConflict::UnregisteredTokenId),
                Some(onchain) if onchain != expected => {
                    conflicts.push_back(TokenMappingConflict::ManagerType(onchain, expected));
                }
                Some(_) => {}
            }
        }

        self.conflicts = conflicts;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address::AddressKind;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::{Env, String};

    /// Real strkeys with real checksums, the same ones the strkey module is tested against.
    const G_ADDR: &str = "GA5ZYIIVYDX5GRF2BKEQD2Y47ZCPG3HX3PUUNPVQ5ZECB57MJG7RLVNR";
    const C_ADDR: &str = "CAGR5KFYMZYI7WWQ6TWYYZ346T7GNZLKER4DOJTAG3SOB46QLR5RAPSN";
    const M_ADDR: &str = "MA5ZYIIVYDX5GRF2BKEQD2Y47ZCPG3HX3PUUNPVQ5ZECB57MJG7RKABAAAAAAAAAAFLXQ";

    fn evm(env: &Env, last: u8) -> BytesN<20> {
        let mut raw = [0x7Cu8; 20];
        raw[19] = last;
        BytesN::from_array(env, &raw)
    }

    fn inbound(env: &Env, kind: AddressKind, value: &str, nonce: u64) -> InboundNote {
        InboundNote::new(
            StrkeyDestination::new(kind, String::from_str(env, value)),
            nonce,
        )
    }

    #[test]
    fn an_outbound_note_is_twenty_nine_bytes_and_reads_back_field_for_field() {
        let env = Env::default();
        let note = OutboundNote::new(evm(&env, 0x11), 4242);
        let wire = note.encode(&env).unwrap();

        assert_eq!(wire.len(), OUTBOUND_NOTE_LEN);
        assert_eq!(wire.get(0).unwrap(), NOTE_VERSION);
        assert_eq!(OutboundNote::decode(&env, &wire).unwrap(), note);
    }

    #[test]
    fn the_outbound_nonce_sits_big_endian_where_a_solidity_slice_expects_it() {
        let env = Env::default();
        // The far side reads this as `uint64(bytes8(payload[21:29]))`. Spelling the bytes out
        // rather than round tripping is the only way this test would notice an endianness flip.
        let wire = OutboundNote::new(evm(&env, 0x11), 0x0102_0304_0506_0708)
            .encode(&env)
            .unwrap();
        for (i, expected) in [1u8, 2, 3, 4, 5, 6, 7, 8].iter().enumerate() {
            assert_eq!(wire.get(21 + i as u32).unwrap(), *expected);
        }
    }

    #[test]
    fn the_twenty_address_bytes_land_flat_rather_than_padded() {
        let env = Env::default();
        let address = evm(&env, 0x99);
        let wire = OutboundNote::new(address.clone(), 1).encode(&env).unwrap();
        // No left padding, no length prefix. A Solidity reader slicing [1:21] gets the address
        // and nothing else, which is the entire point of a packed layout.
        assert_eq!(
            wire.slice(1..21),
            Bytes::from_array(&env, &address.to_array())
        );
    }

    #[test]
    fn an_outbound_note_naming_nobody_is_refused_on_the_way_out_and_on_the_way_in() {
        let env = Env::default();
        let zero = BytesN::from_array(&env, &[0u8; 20]);
        assert_eq!(
            OutboundNote::new(zero, 1).encode(&env),
            Err(HyperionError::ZeroAddressKey)
        );

        // And built by hand, because a payload arriving from somewhere else was not built by us.
        let mut raw = Bytes::new(&env);
        raw.push_back(NOTE_VERSION);
        raw.append(&Bytes::from_array(&env, &[0u8; 28]));
        assert_eq!(
            OutboundNote::decode(&env, &raw),
            Err(HyperionError::ZeroAddressKey)
        );
    }

    #[test]
    fn an_outbound_note_one_byte_out_is_refused_rather_than_read_past_the_end() {
        let env = Env::default();
        let full = OutboundNote::new(evm(&env, 0x11), 7).encode(&env).unwrap();

        assert_eq!(
            OutboundNote::decode(&env, &full.slice(0..OUTBOUND_NOTE_LEN - 1)),
            Err(HyperionError::MalformedMessage)
        );

        let mut long = full.clone();
        long.push_back(0);
        assert_eq!(
            OutboundNote::decode(&env, &long),
            Err(HyperionError::MalformedMessage)
        );
    }

    #[test]
    fn all_three_kinds_of_stellar_destination_survive_the_inbound_round_trip() {
        let env = Env::default();
        for (kind, value) in [
            (AddressKind::Account, G_ADDR),
            (AddressKind::Contract, C_ADDR),
            (AddressKind::MuxedAccount, M_ADDR),
        ] {
            let note = inbound(&env, kind, value, 11);
            let wire = note.encode(&env).unwrap();
            assert_eq!(InboundNote::decode(&env, &wire).unwrap(), note);
        }
    }

    #[test]
    fn a_muxed_inbound_note_is_longer_and_still_finds_its_nonce() {
        let env = Env::default();
        // The nonce is read from the end rather than a fixed offset, so a destination that
        // changes length must not move it. This is the case that would catch it if it did.
        let plain = inbound(&env, AddressKind::Account, G_ADDR, u64::MAX);
        let muxed = inbound(&env, AddressKind::MuxedAccount, M_ADDR, u64::MAX);
        let plain_wire = plain.encode(&env).unwrap();
        let muxed_wire = muxed.encode(&env).unwrap();

        assert_eq!(muxed_wire.len(), plain_wire.len() + 13);
        assert_eq!(
            InboundNote::decode(&env, &plain_wire).unwrap().nonce,
            u64::MAX
        );
        assert_eq!(
            InboundNote::decode(&env, &muxed_wire).unwrap().nonce,
            u64::MAX
        );
    }

    #[test]
    fn a_note_from_a_version_we_do_not_speak_is_refused_outright() {
        let env = Env::default();
        let mut outbound = OutboundNote::new(evm(&env, 1), 1).encode(&env).unwrap();
        let tail = outbound.slice(1..outbound.len());
        outbound = Bytes::from_array(&env, &[NOTE_VERSION + 1]);
        outbound.append(&tail);
        assert_eq!(
            OutboundNote::decode(&env, &outbound),
            Err(HyperionError::UnsupportedHookVersion)
        );

        let encoded = inbound(&env, AddressKind::Account, G_ADDR, 1)
            .encode(&env)
            .unwrap();
        let mut bumped = Bytes::from_array(&env, &[9u8]);
        bumped.append(&encoded.slice(1..encoded.len()));
        assert_eq!(
            InboundNote::decode(&env, &bumped),
            Err(HyperionError::UnsupportedHookVersion)
        );
    }

    #[test]
    fn a_truncated_inbound_note_never_becomes_a_shorter_valid_one() {
        let env = Env::default();
        let full = inbound(&env, AddressKind::Account, G_ADDR, 5)
            .encode(&env)
            .unwrap();
        // Chop a byte at a time off the end. Every single one of these has to fail, because a
        // strkey that parses after being cut short is a different address than was sent.
        let mut cut = 1u32;
        while cut < full.len() {
            assert!(InboundNote::decode(&env, &full.slice(0..full.len() - cut)).is_err());
            cut += 1;
        }
    }

    #[test]
    fn an_inbound_note_carrying_a_strkey_that_lies_about_its_kind_is_refused() {
        let env = Env::default();
        // A G address tagged as a contract. The checksum is perfect and the string is real; the
        // only thing wrong is that the sender and the address disagree, which is exactly the
        // disagreement a bridge must not resolve on its own.
        let note = inbound(&env, AddressKind::Contract, G_ADDR, 1);
        assert_eq!(note.encode(&env), Err(HyperionError::InvalidDestination));
    }

    #[test]
    fn a_trustline_is_needed_for_the_kinds_that_need_one_and_not_for_contracts() {
        let env = Env::default();
        assert!(inbound(&env, AddressKind::Account, G_ADDR, 1).needs_trustline());
        assert!(inbound(&env, AddressKind::MuxedAccount, M_ADDR, 1).needs_trustline());
        assert!(!inbound(&env, AddressKind::Contract, C_ADDR, 1).needs_trustline());
    }

    #[test]
    fn both_directions_share_the_version_byte_the_cctp_hook_uses() {
        // One envelope version across both rails. If these ever diverge it should be because
        // somebody changed one deliberately, and this is the test that makes them notice.
        assert_eq!(NOTE_VERSION, crate::cctp::HOOK_VERSION);
    }

    // -------------------------------------------------------------------------------------
    // Registration conflict classification
    // -------------------------------------------------------------------------------------

    /// A report for an asset ITS knows about and backs with that same asset, which is the only
    /// starting point from which "compatible" is a meaningful thing to assert.
    fn report(env: &Env) -> TokenMappingReport {
        let mut report = TokenMappingReport::new(
            env,
            Address::from_string(&String::from_str(
                env,
                "CAGR5KFYMZYI7WWQ6TWYYZ346T7GNZLKER4DOJTAG3SOB46QLR5RAPSN",
            )),
            BytesN::from_array(env, &[0x11; 32]),
        );
        report.rail_token = Some(report.token.clone());
        report
    }

    /// A second real address, so a conflict between two of them is a conflict between two
    /// checksums that both verify rather than one that was typed wrong.
    fn other_address(env: &Env) -> Address {
        Address::from_string(&String::from_str(env, G_ADDR))
    }

    #[test]
    fn a_report_that_matches_the_rail_everywhere_is_compatible() {
        let env = Env::default();
        let mut report = report(&env);
        report.classify(&env);
        assert!(report.is_compatible(), "{:?}", report.conflicts);
        assert_eq!(report.conflicts.len(), 0);
    }

    #[test]
    fn an_id_axelar_does_not_back_is_a_conflict_even_with_nothing_else_observed() {
        // A registration whose rail half cannot be read is not "compatible with no findings".
        // There is nothing to route through, so it is the first thing worth saying.
        let env = Env::default();
        let mut report = TokenMappingReport::new(
            &env,
            Address::from_string(&String::from_str(
                &env,
                "CAGR5KFYMZYI7WWQ6TWYYZ346T7GNZLKER4DOJTAG3SOB46QLR5RAPSN",
            )),
            BytesN::from_array(&env, &[0x11; 32]),
        );
        report.classify(&env);
        assert_eq!(
            report.conflicts,
            Vec::from_array(&env, [TokenMappingConflict::UnregisteredTokenId])
        );
    }

    #[test]
    fn a_mapping_that_reads_back_as_proposed_is_not_a_conflict() {
        // The repeat case. Both stored values match what was asked for, so there is nothing to
        // report and `map_token` can treat it as a no-op rather than a rewrite.
        let env = Env::default();
        let mut report = report(&env);
        report.mapped_token_id = Some(report.token_id.clone());
        report.mapped_token = Some(report.token.clone());
        report.rail_token = Some(report.token.clone());
        report.rail_manager_type = Some(4);
        report.onchain_decimals = Some(7);
        report.expected_decimals = Some(7);
        report.expected_manager_type = Some(4);
        report.classify(&env);
        assert!(report.is_compatible(), "{:?}", report.conflicts);
    }

    #[test]
    fn both_directions_of_a_token_conflict_are_named_with_both_values() {
        let env = Env::default();
        let mut report = report(&env);
        let stranger = other_address(&env);
        report.mapped_token_id = Some(BytesN::from_array(&env, &[0x22; 32]));
        report.mapped_token = Some(stranger.clone());
        report.rail_token = Some(report.token.clone());
        report.classify(&env);
        assert_eq!(
            report.conflicts,
            Vec::from_array(
                &env,
                [
                    TokenMappingConflict::TokenId(
                        BytesN::from_array(&env, &[0x22; 32]),
                        BytesN::from_array(&env, &[0x11; 32]),
                    ),
                    TokenMappingConflict::LocalToken(stranger, report.token.clone()),
                ]
            )
        );
    }

    #[test]
    fn an_unknown_token_id_and_a_mismatched_rail_token_are_distinguished() {
        // Two different mistakes with two different fixes, so they get two different names. An
        // operator told "conflict" for an id Axelar has never seen would go looking for a second
        // mapping that does not exist.
        let env = Env::default();
        let mut unknown = report(&env);
        unknown.rail_token = None;
        unknown.classify(&env);
        assert_eq!(
            unknown.conflicts,
            Vec::from_array(&env, [TokenMappingConflict::UnregisteredTokenId])
        );

        let mut mismatched = report(&env);
        mismatched.rail_token = Some(other_address(&env));
        mismatched.classify(&env);
        assert_eq!(
            mismatched.conflicts,
            Vec::from_array(
                &env,
                [TokenMappingConflict::RailTokenMismatch(
                    other_address(&env),
                    mismatched.token.clone()
                )]
            )
        );
    }

    #[test]
    fn decimals_and_manager_kind_are_checked_only_when_the_caller_states_an_expectation() {
        let env = Env::default();
        // Observed but unjudged: an operator checking the ids should not have to guess at the
        // other two fields to get a useful answer.
        let mut observed = report(&env);
        observed.onchain_decimals = Some(6);
        observed.rail_manager_type = Some(2);
        observed.classify(&env);
        assert!(observed.is_compatible(), "{:?}", observed.conflicts);

        let mut judged = report(&env);
        judged.onchain_decimals = Some(6);
        judged.rail_manager_type = Some(2);
        judged.expected_decimals = Some(7);
        judged.expected_manager_type = Some(4);
        judged.classify(&env);
        assert_eq!(
            judged.conflicts,
            Vec::from_array(
                &env,
                [
                    TokenMappingConflict::Decimals(6, 7),
                    TokenMappingConflict::ManagerType(2, 4),
                ]
            )
        );
    }

    #[test]
    fn unreadable_metadata_is_its_own_finding_rather_than_a_decimals_mismatch() {
        let env = Env::default();
        let mut report = report(&env);
        report.expected_decimals = Some(7);
        report.classify(&env);
        assert_eq!(
            report.conflicts,
            Vec::from_array(&env, [TokenMappingConflict::UnreadableMetadata])
        );
    }

    #[test]
    fn outbound_reconciliation_verifies_on_exact_match() {
        let env = Env::default();
        let note = OutboundNote::new(evm(&env, 0x01), 42);
        let token_id = BytesN::from_array(&env, &[0xAAu8; 32]);
        let amount = 10_000_000i128;
        let command_id = BytesN::from_array(&env, &[0x55u8; 32]);

        let candidate = CandidatePayment {
            command_id: command_id.clone(),
            token_id: token_id.clone(),
            amount,
            recipient: evm(&env, 0x01),
        };

        assert_eq!(
            verify_outbound_reconciliation(&note, &token_id, amount, &command_id, &candidate),
            Ok(())
        );
    }

    #[test]
    fn outbound_reconciliation_detects_mismatches() {
        let env = Env::default();
        let note = OutboundNote::new(evm(&env, 0x01), 42);
        let token_id = BytesN::from_array(&env, &[0xAAu8; 32]);
        let amount = 10_000_000i128;
        let command_id = BytesN::from_array(&env, &[0x55u8; 32]);

        // Recipient mismatch
        let mut candidate = CandidatePayment {
            command_id: command_id.clone(),
            token_id: token_id.clone(),
            amount,
            recipient: evm(&env, 0x02),
        };
        assert_eq!(
            verify_outbound_reconciliation(&note, &token_id, amount, &command_id, &candidate),
            Err(ReconciliationDiscrepancy::RecipientMismatch)
        );

        // Token ID mismatch
        candidate.recipient = evm(&env, 0x01);
        candidate.token_id = BytesN::from_array(&env, &[0xBBu8; 32]);
        assert_eq!(
            verify_outbound_reconciliation(&note, &token_id, amount, &command_id, &candidate),
            Err(ReconciliationDiscrepancy::TokenIdMismatch)
        );

        // Amount mismatch
        candidate.token_id = token_id.clone();
        candidate.amount = 9_999_999i128;
        assert_eq!(
            verify_outbound_reconciliation(&note, &token_id, amount, &command_id, &candidate),
            Err(ReconciliationDiscrepancy::AmountMismatch)
        );

        // A nonempty destination identity still cannot establish the source command.
        candidate.amount = amount;
        candidate.command_id = BytesN::from_array(&env, &[0x66; 32]);
        assert_eq!(
            verify_outbound_reconciliation(&note, &token_id, amount, &command_id, &candidate),
            Err(ReconciliationDiscrepancy::CommandIdentityMismatch)
        );
        candidate.command_id = command_id.clone();
        candidate.amount = -1;
        assert_eq!(
            verify_outbound_reconciliation(&note, &token_id, -1, &command_id, &candidate),
            Err(ReconciliationDiscrepancy::InvalidAmount)
        );

        // Missing command ID
        candidate.amount = amount;
        candidate.command_id = BytesN::from_array(&env, &[0u8; 32]);
        assert_eq!(
            verify_outbound_reconciliation(&note, &token_id, amount, &command_id, &candidate),
            Err(ReconciliationDiscrepancy::CommandIdentityMissing)
        );
    }

    #[test]
    fn inbound_reconciliation_verifies_and_detects_mismatches() {
        let env = Env::default();
        let note = inbound(&env, AddressKind::Account, G_ADDR, 99);
        let token_id = BytesN::from_array(&env, &[0xAAu8; 32]);
        let amount = 5_000_000i128;
        let message_id = String::from_str(&env, "0xfeed-1");

        let candidate = CandidateInboundPayment {
            message_id: message_id.clone(),
            token_id: token_id.clone(),
            amount,
            recipient: StrkeyDestination::new(AddressKind::Account, String::from_str(&env, G_ADDR)),
        };

        assert_eq!(
            verify_inbound_reconciliation(&note, &token_id, amount, &message_id, &candidate),
            Ok(())
        );

        let wrong_source_id = String::from_str(&env, "different-complete-source-id");
        assert_eq!(
            verify_inbound_reconciliation(&note, &token_id, amount, &wrong_source_id, &candidate),
            Err(ReconciliationDiscrepancy::CommandIdentityMismatch)
        );

        // Missing message ID
        let empty_candidate = CandidateInboundPayment {
            message_id: String::from_str(&env, ""),
            token_id: token_id.clone(),
            amount,
            recipient: candidate.recipient.clone(),
        };
        assert_eq!(
            verify_inbound_reconciliation(&note, &token_id, amount, &message_id, &empty_candidate),
            Err(ReconciliationDiscrepancy::CommandIdentityMissing)
        );
    }

    #[test]
    fn reconciliation_requires_source_binding_and_positive_amounts() {
        let env = Env::default();
        let note = OutboundNote::new(evm(&env, 1), 42);
        let token = BytesN::from_array(&env, &[0xaa; 32]);
        let command = BytesN::from_array(&env, &[0x55; 32]);
        let empty = BytesN::from_array(&env, &[0; 32]);
        let candidate = CandidatePayment {
            command_id: command.clone(),
            token_id: token.clone(),
            amount: 1,
            recipient: note.recipient.clone(),
        };
        assert_eq!(
            verify_outbound_reconciliation(&note, &token, 1, &empty, &candidate),
            Err(ReconciliationDiscrepancy::CommandIdentityMissing)
        );
        let empty_token_candidate = CandidatePayment {
            token_id: empty.clone(),
            ..candidate.clone()
        };
        assert_eq!(
            verify_outbound_reconciliation(&note, &empty, 1, &command, &empty_token_candidate),
            Err(ReconciliationDiscrepancy::InvalidTokenIdentity)
        );
        let zero_candidate = CandidatePayment {
            amount: 0,
            ..candidate
        };
        assert_eq!(
            verify_outbound_reconciliation(&note, &token, 0, &command, &zero_candidate),
            Err(ReconciliationDiscrepancy::InvalidAmount)
        );
        for invalid in ["", " ", "ethereum-transaction-17\n"] {
            assert!(!valid_message_identity(&String::from_str(&env, invalid)));
        }
        assert!(valid_message_identity(&String::from_str(
            &env,
            "ethereum-transaction-17"
        )));
    }

    #[test]
    fn minimum_peer_rotation_delay_is_at_least_one_hour() {
        assert_eq!(MIN_PEER_ROTATION_DELAY, 3_600);
    }

    #[test]
    fn auth_tree_snapshot_verifies_exact_match_and_detects_privilege_expansion() {
        let env = Env::default();
        let user = Address::generate(&env);
        let token = Address::generate(&env);
        let manager = Address::generate(&env);
        let treasury = Address::generate(&env);
        let gas_token = Address::generate(&env);
        let its = Address::generate(&env);

        let baseline = PaidDispatchAuthSnapshot {
            sender: user.clone(),
            principal_token: token.clone(),
            principal_spender: manager.clone(),
            principal_amount: 10_000_000,
            is_burn: false,
            fee_token: token.clone(),
            fee_recipient: treasury.clone(),
            fee_amount: 50_000,
            gas_token: gas_token.clone(),
            gas_recipient: its.clone(),
            gas_amount: 1_000,
        };

        // Exact match passes
        assert_eq!(inspect_auth_tree_snapshot(&baseline, &baseline), Ok(()));

        // Changed sender
        let mut candidate = baseline.clone();
        candidate.sender = Address::generate(&env);
        assert_eq!(
            inspect_auth_tree_snapshot(&baseline, &candidate),
            Err(PrivilegeExpansion::SenderMismatch)
        );

        // Changed principal spender
        candidate = baseline.clone();
        candidate.principal_spender = Address::generate(&env);
        assert_eq!(
            inspect_auth_tree_snapshot(&baseline, &candidate),
            Err(PrivilegeExpansion::ChangedPrincipalSpender)
        );

        // Changed gas recipient
        candidate = baseline.clone();
        candidate.gas_recipient = Address::generate(&env);
        assert_eq!(
            inspect_auth_tree_snapshot(&baseline, &candidate),
            Err(PrivilegeExpansion::ChangedGasRecipient)
        );

        // Changed fee recipient
        candidate = baseline.clone();
        candidate.fee_recipient = Address::generate(&env);
        assert_eq!(
            inspect_auth_tree_snapshot(&baseline, &candidate),
            Err(PrivilegeExpansion::ChangedFeeRecipient)
        );

        // Principal amount expansion
        candidate = baseline.clone();
        candidate.principal_amount = 10_000_001;
        assert_eq!(
            inspect_auth_tree_snapshot(&baseline, &candidate),
            Err(PrivilegeExpansion::PrincipalAmountMismatch)
        );

        // Gas amount expansion
        candidate = baseline.clone();
        candidate.gas_amount = 2_000;
        assert_eq!(
            inspect_auth_tree_snapshot(&baseline, &candidate),
            Err(PrivilegeExpansion::GasAmountMismatch)
        );
    }
}
