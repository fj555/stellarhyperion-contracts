// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @dev Which rail a transfer travels on.
///
/// The ordering is not cosmetic. These values are the same integers `hyperion_core::RouteKind`
/// encodes on the Stellar side, so a route read off an event on one chain means the same thing
/// when an indexer reads it on the other. Appending is fine; reordering is not.
enum RouteKind {
    Cctp,
    AxelarIts,
    AxelarGmp,
    Allbridge
}

/// @dev What kind of Stellar address a 32 byte key describes.
///
/// Stellar has three, and a cross-chain message carries a bare key with nothing to say which.
/// `G` is a classic ed25519 account, `C` a Soroban contract, `M` a SEP-23 muxed account that
/// shares one underlying `G` with thousands of others. Paying the wrong kind is not a reverted
/// transaction, it is money delivered somewhere nobody holds a key for, so the kind travels
/// alongside the key everywhere in this codebase.
enum AddressKind {
    Account,
    Contract,
    MuxedAccount
}

/// @dev Where a transfer is going.
///
/// The address travels as the string a person copies out of their wallet rather than as a
/// pre-split key, because a strkey carries its own CRC16 and splitting it somewhere else throws
/// that checksum away. `StellarAddress.parse` verifies it here, before anything moves.
struct Destination {
    /// Hyperion's own name for the chain, which the adapter translates into whatever the rail
    /// calls it. Lowercase and stable, so "stellar" rather than "Stellar" or domain 27.
    string chain;
    /// Fifty six characters starting with G or C, or sixty nine starting with M.
    string strkey;
}

/// @dev A request to send funds out of this chain.
struct OutboundRequest {
    address token;
    uint256 amount;
    RouteKind route;
    Destination destination;
    /// How many decimal places the asset has where it lands. Six on EVM, seven on Stellar for
    /// most issued assets, and getting it wrong is a factor of ten rather than a revert.
    uint8 destinationDecimals;
    /// The least the sender will accept arriving. Zero means they did not say.
    uint256 minDestinationAmount;
}

/// @dev Everything the router knows about an asset it is willing to move.
struct TokenConfig {
    bool registered;
    uint8 decimals;
    /// Ceiling per flow window, in this chain's units for this asset.
    uint256 flowLimit;
    bool enabled;
}

/// @dev Notes on two structs that exist on the Stellar side and deliberately do not exist here.
///
/// `TransferRecord` and `InboundRecord` are stored contract state in the Soroban router, because
/// Soroban events are not retained for long and an indexer that falls behind has nowhere else to
/// look. On this chain a log is permanent and indexable by topic, so the same information is
/// emitted and not written to storage. Paying sixty thousand gas per transfer to duplicate a log
/// into a mapping nothing on chain ever reads would be a cost with no reader.

/// @dev Funds that arrived but could not be handed over yet.
///
/// A parked claim is the answer to a token that refuses a transfer to a particular address.
/// USDC can freeze an account, and a frozen recipient must not be able to wedge an attested
/// delivery: the rail has already burned the counterpart on the far side, so reverting would
/// destroy the money rather than delay it. The funds sit here instead and anybody may settle
/// the claim later, which needs no privileges because the recipient was fixed when the rail
/// signed the message.
struct PendingClaim {
    uint64 id;
    address recipient;
    address token;
    uint256 amount;
    RouteKind route;
    string sourceChain;
    uint64 sourceNonce;
    uint64 createdAt;
    bool settled;
}

/// @dev Versioned operational result for one requested claim settlement.
enum ClaimSettlementStatus {
    Settled,
    NotFound,
    AlreadySettled,
    RecipientRestricted,
    InsufficientRouterBalance,
    TokenUnavailable,
    InvalidTokenPayout
}

struct ClaimSettlementResult {
    uint64 id;
    ClaimSettlementStatus status;
}

/// @dev Where an inbound delivery says it came from.
struct Origin {
    string chain;
    /// The far side router's own sequence number, for humans and indexers.
    uint64 nonce;
    /// The rail's own identifier for this message, at full width. This is what the replay guard
    /// is keyed on, because squeezing a 32 byte CCTP nonce or an Axelar message id into a uint64
    /// lets two different messages collide on one key.
    bytes32 messageId;
    bytes32 sender;
}

/// @dev The administrative changes that exist. Anything not on this list cannot be done at all.
enum ActionKind {
    SetFeeBps,
    SetTreasury,
    SetAdmin,
    SetGuardian,
    SetAdapter,
    SetRailReceiver,
    EnableRoute,
    RegisterToken,
    RaiseTokenFlowLimit,
    SetRouteFlowLimit,
    SetTimelockDelay,
    SetFlowWindow,
    /// Retire an asset without unregistering it, or bring one back.
    ///
    /// Separate from the flow limit on purpose. Dropping a limit to zero is the guardian's
    /// emergency brake and it reports itself as a flow refusal, which is the right thing to say
    /// while an incident is running. Retiring an asset for good is a different decision, it
    /// waits out the timelock like every other one, and it should say so plainly to anybody
    /// reading a failed quote.
    SetTokenEnabled
}

/// @dev One administrative change, in a shape a reviewer can read as fast as the contract can.
///
/// Flat and fixed rather than an encoded blob, and every field a kind does not use has to be
/// zero. A timelock exists so somebody can read a pending change and understand it before it
/// lands, and a struct with a spare field carrying a value nobody validates is a change that
/// reads one way to a person and another way to the code.
struct AdminAction {
    ActionKind kind;
    RouteKind route;
    /// The token for the token scoped kinds, otherwise the address being set.
    address subject;
    /// Basis points, a flow ceiling, a delay in seconds, or a window in seconds.
    uint256 amount;
    /// Decimal places, and only `RegisterToken` uses it.
    uint8 decimals;
}

/// @dev A change that has been announced and is waiting out its delay.
struct QueuedAction {
    uint64 id;
    AdminAction action;
    uint64 queuedAt;
    /// The earliest it may be executed.
    uint64 eta;
    /// The point after which it has to be queued again, so a forgotten change does not sit
    /// executable forever.
    uint64 expiresAt;
    bool executed;
}

/// @dev What a route would do with a given amount right now, without doing it.
struct RouteQuote {
    RouteKind route;
    bool available;
    /// Why not, when `available` is false. Zero when it is true.
    QuoteBlocker reason;
    uint256 grossAmount;
    uint256 fee;
    uint256 netAmount;
    /// What lands, in the destination's own decimal base.
    uint256 destinationAmount;
    uint256 flowAvailable;
    /// True for rails whose second leg waits on an attestation somebody has to fetch.
    bool waitsOnAttestation;
    /// True for the rail that moves the asset itself rather than a wrapper of it.
    bool isCanonical;
}

/// @dev Structured reason code when a queued timelock action is cancelled.
enum CancelReason {
    Unspecified,
    Superseded,
    ParametersIncorrect,
    SecurityConcern,
    Expired,
    OperatorAborted
}

/// @dev Lifecycle status of a timelock action in its audit record.
enum ActionAuditStatus {
    Queued,
    Executed,
    Cancelled
}

/// @dev Why a route turned a quote down.
enum QuoteBlocker {
    None,
    Paused,
    RouteDisabled,
    AdapterNotSet,
    TokenNotRegistered,
    TokenDisabled,
    AmountTooSmall,
    FlowLimitExceeded,
    NotRepresentable,
    MuxedNotSupported,
    InvalidDestination,
    ChainNotSupported
}

// Additive V2 result: the legacy RouteQuote tuple and its selectors remain unchanged.
struct RouteQuoteV2 {
    RouteQuote quote;
    uint256 requestedAmount;
    uint256 fee;
    uint256 netAmount;
    uint256 retainedDust;
    uint256 actualDebit;
    bool breakdownKnown;
}

/// @dev One way a proposed token registration disagrees with what is already there.
///
/// The names and the order match `hyperion_core::axelar::TokenMappingConflict` variant for
/// variant, because the two chains print the same diagnostic and an operator should not have to
/// learn a second vocabulary to read the other half. `None` exists only so a zero value is not a
/// claim about a conflict that was never found.
enum TokenMappingConflict {
    None,
    TokenId,
    LocalToken,
    Decimals,
    ManagerType,
    UnreadableMetadata,
    UnregisteredTokenId,
    RailTokenMismatch
}

/// @dev What registering a token would do, without doing any of it.
///
/// Every observed field travels alongside the verdict, so a dry run can print the whole picture
/// rather than a single refusal. `conflicts` is empty when the mapping can be activated as it
/// stands, and each entry names exactly one field that disagrees.
struct TokenMappingReport {
    /// The local asset the caller proposed.
    address token;
    /// The token id the caller proposed.
    bytes32 tokenId;
    /// The token id already linked to `token`, or zero when there is none.
    bytes32 mappedTokenId;
    /// The local asset already linked to `tokenId`, or zero when there is none.
    address mappedToken;
    /// The asset Axelar's own registry reports for `tokenId`, or zero when it knows no such id.
    address railToken;
    /// The token manager Axelar would move funds through, or zero when none is deployed.
    address railManager;
    /// How that manager takes funds, as Axelar's `TokenManagerType` discriminant.
    uint256 railManagerType;
    /// Whether `railManagerType` was actually read. Zero is a real kind, so the discriminant
    /// alone cannot say whether a manager answered at all.
    bool managerTypeKnown;
    /// Decimals the local asset reports, when it answers `decimals()` at all.
    uint8 onchainDecimals;
    /// Whether `onchainDecimals` was actually read, rather than defaulting to zero.
    bool decimalsKnown;
    /// Every disagreement found, in the same fixed order the Stellar side uses.
    TokenMappingConflict[] conflicts;
}

/// @dev What a caller expects a registration to find, so the check can judge it rather than
/// merely describe it. The two `has` flags exist because zero is a legitimate manager type and a
/// legitimate decimal count, so absence cannot be encoded as zero.
struct TokenMappingRequest {
    address token;
    bytes32 tokenId;
    uint8 expectedDecimals;
    bool hasExpectedDecimals;
    uint256 expectedManagerType;
    bool hasExpectedManagerType;
}
