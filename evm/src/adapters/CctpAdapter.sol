// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Ownable} from "openzeppelin/access/Ownable.sol";
import {Ownable2Step} from "openzeppelin/access/Ownable2Step.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";
import {SafeERC20} from "openzeppelin/token/ERC20/utils/SafeERC20.sol";

import {
    AlreadyConfigured,
    DecimalOverflow,
    InvalidAmount,
    RailNotConfigured,
    RefundFailed,
    TimelockDelayOutOfRange,
    TimelockNotQueued,
    TimelockNotReady,
    TokenNotMapped,
    Unauthorized,
    UnknownChain,
    ZeroAddress
} from "../HyperionErrors.sol";
import {AddressKind, Destination, RouteKind} from "../HyperionTypes.sol";
import {ITokenMessengerV2} from "../interfaces/ICctpV2.sol";
import {IRailAdapter} from "../interfaces/IRailAdapter.sol";
import {HyperionNotes} from "../libraries/HyperionNotes.sol";
import {StellarAddress} from "../libraries/StellarAddress.sol";

/// @title Hyperion over Circle's CCTP V2
/// @author dotmantissa
/// @notice The rail people actually want when they are moving dollars. USDC is burned here and
/// minted there, natively, with no pool to run thin and no wrapper to unwrap. The cost is the
/// wait: Circle's attestation service has to sign the burn before anything can mint, and asking
/// for finalized rather than fast means waiting for the source chain to finalize too.
///
/// This adapter only sends. That is not a gap, it is the shape of the integration. Going the
/// other way, Stellar to here, the Soroban side sets CCTP's mint recipient to the user's own EVM
/// address and attaches no hook at all, so USDC mints straight into their wallet and Hyperion is
/// not in the path. There is nothing for a contract on this chain to do, so there is no contract.
///
/// Going this way the mint recipient has to be Hyperion's adapter on Stellar instead, because
/// CCTP's recipient field is thirty two flat bytes with nowhere to record whether they name a
/// classic account or a Soroban contract, and on Stellar those are two completely different kinds
/// of delivery. So the recipient names Hyperion over there and the real destination travels in
/// the hook, where there is room to say which of the three kinds it is.
///
/// Note on deployment gating: The active deployment is Stellar Testnet outbound ITS/CCTP.
/// EVM and other rail development paths remain explicitly gated until independently validated.
/// Compatibility with provider upgrades requires explicit message-boundary tooling and approval
/// lifecycle reconciliation.
contract CctpAdapter is IRailAdapter, Ownable2Step {
    using SafeERC20 for IERC20;

    /// @notice Version for CCTP approval diagnostics.
    uint32 public constant APPROVAL_DIAGNOSTICS_VERSION = 1;

    enum ProviderReadStatus {
        Unknown,
        Usable,
        Paused
    }

    struct LaneReadiness {
        uint32 version;
        bool routerPaused;
        ProviderReadStatus messenger;
        ProviderReadStatus transmitter;
        address messengerContract;
        address transmitterContract;
        bool laneAvailable;
    }

    enum AttestationCondition {
        Unverified,
        Verified,
        Expired,
        UnsupportedRecovery
    }

    struct RenewalPlan {
        uint32 version;
        AttestationCondition condition;
        bytes32 sourceMessageId;
        bytes32 payoutIntent;
        uint256 expiresAt;
        bool recoverySupported;
    }

    /// @notice CCTP message transmitter used to attest provider lane readiness.
    address public messageTransmitter;

    /// @notice Expected residual allowance on the token messenger.
    uint256 public constant DOCUMENTED_RESIDUAL_ALLOWANCE = 0;

    /// @notice Findings produced by the allowance auditor.
    enum ApprovalFinding {
        Clean,
        UnexpectedResidual,
        Unavailable
    }

    /// @notice Versioned diagnostics snapshot of the allowance state.
    struct ApprovalDiagnostics {
        uint32 version;
        address token;
        address spender;
        // Zero is a placeholder when finding is Unavailable.
        uint256 allowance;
        uint256 expectedAllowance;
        bool isClean;
        ApprovalFinding finding;
    }

    /// @notice The confirmation level every burn asks Circle for.
    /// @dev Finalized rather than fast. Fast confirmation trades reorg risk for about fifteen
    /// minutes, and it is not this contract's fifteen minutes or its risk.
    uint32 public constant FINALITY_THRESHOLD_FINALIZED = 2000;

    /// @notice The router, and the only address allowed to push anything onto this rail.
    address public immutable ROUTER;

    /// @notice Circle's TokenMessenger on this chain.
    ITokenMessengerV2 public immutable TOKEN_MESSENGER;

    /// @notice The only token CCTP carries.
    address public immutable USDC;

    /// @notice One configured destination.
    struct Lane {
        /// Circle's domain number for the far side. Stellar is 27.
        uint32 domain;
        /// Hyperion's own adapter over there, in CCTP's thirty two byte form.
        bytes32 mintRecipient;
        bool configured;
        bool enabled;
    }

    /// @notice Minimum delay before a proposed peer rotation can become active (1 hour).
    uint64 public constant MIN_ROTATION_DELAY = 1 hours;

    /// @notice Default overlap acceptance window for historical in-flight messages (24 hours).
    uint64 public constant DEFAULT_OVERLAP_WINDOW = 1 days;

    /// @notice A proposed peer rotation awaiting the timelock delay.
    struct PeerRotation {
        bytes32 newMintRecipient;
        uint64 eta;
        uint64 queuedAt;
    }

    /// @notice Historical peer window for retired peer messenger.
    struct HistoricalPeer {
        bytes32 oldMintRecipient;
        uint64 validUntil;
    }

    mapping(string chain => Lane lane) private _lanes;
    mapping(string chain => PeerRotation) private _pendingRotations;
    mapping(string chain => HistoricalPeer) private _historicalPeers;

    /// @notice A destination became reachable.
    event LaneConfigured(string chain, uint32 domain, bytes32 mintRecipient);

    /// @notice A destination was turned on or off.
    event LaneEnabled(string chain, bool enabled);

    /// @notice A peer messenger rotation was proposed.
    event PeerRotationQueued(string indexed chain, bytes32 currentPeer, bytes32 newPeer, uint64 eta);

    /// @notice A peer messenger rotation was applied.
    event PeerRotationApplied(string indexed chain, bytes32 oldPeer, bytes32 newPeer, uint64 overlapDeadline);

    /// @notice A peer messenger rotation was cancelled.
    event PeerRotationCancelled(string indexed chain, bytes32 cancelledPeer);

    /// @notice A burn was handed to Circle.
    event BurnSubmitted(
        string chain, uint32 domain, uint256 amount, bytes32 mintRecipient, uint64 nonce, bytes hookData
    );

    /// @notice Effective finality policy applied to a submitted burn.
    event FinalityPolicyUsed(string chain, uint32 indexed domain, uint32 minFinalityThreshold);

    /// @notice Diagnostics announced for CCTP approval lifecycle during dispatch.
    event ApprovalAudited(
        address indexed token,
        address indexed spender,
        uint256 allowanceBefore,
        uint256 allowanceAfter,
        bool isClean
    );

    /// @notice Emitted when the configured CCTP message transmitter changes.
    event MessageTransmitterSet(address indexed transmitter);

    constructor(address router, address tokenMessenger, address usdc, address owner_) Ownable(owner_) {
        if (router == address(0) || tokenMessenger == address(0) || usdc == address(0)) {
            revert ZeroAddress();
        }
        ROUTER = router;
        TOKEN_MESSENGER = ITokenMessengerV2(tokenMessenger);
        USDC = usdc;
    }

    /// @inheritdoc IRailAdapter
    function route() external pure override returns (RouteKind) {
        return RouteKind.Cctp;
    }

    /// @inheritdoc IRailAdapter
    function supportsChain(string calldata chain) external view override returns (bool) {
        Lane storage lane = _lanes[chain];
        return lane.configured && lane.enabled;
    }

    /// @inheritdoc IRailAdapter
    /// @dev Zero, and it will stay zero. CCTP bills nothing at send time and takes its cut, when
    /// there is one, out of the transferred amount on the far side. Asking for finalized puts this
    /// in the tier where there is not one.
    function quoteFee(string calldata, uint256) external pure override returns (uint256) {
        return 0;
    }

    /// @inheritdoc IRailAdapter
    function checkDispatch(address token, uint256 amount, Destination calldata destination)
        external
        view
        override
    {
        _dispatchPlan(token, amount, destination);
    }

    /// @notice Quote the effective policy for a configured and enabled lane.
    /// @dev This development adapter supports finalized transfers only.
    function quoteFinalityPolicy(string calldata chain) external view returns (uint32) {
        Lane storage lane = _lanes[chain];
        if (!lane.configured) revert UnknownChain();
        if (!lane.enabled) revert RailNotConfigured();
        return FINALITY_THRESHOLD_FINALIZED;
    }

    function _dispatchPlan(address token, uint256 amount, Destination calldata destination)
        private
        view
        returns (Lane memory lane, bytes memory hookData)
    {
        if (token != USDC) revert TokenNotMapped();
        if (amount == 0) revert InvalidAmount();

        lane = _lanes[destination.chain];
        if (!lane.configured) revert UnknownChain();
        if (!lane.enabled) revert RailNotConfigured();

        (AddressKind kind, bytes32 key, uint64 muxedId) = StellarAddress.parse(destination.strkey);
        hookData = HyperionNotes.encodeCctpHook(kind, key, muxedId);
    }

    /// @inheritdoc IRailAdapter
    function dispatch(address token, uint256 amount, Destination calldata destination, uint64 nonce)
        external
        payable
        override
        returns (bytes32)
    {
        if (msg.sender != ROUTER) revert Unauthorized();
        (Lane memory lane, bytes memory hookData) = _dispatchPlan(token, amount, destination);
        emit BurnSubmitted(destination.chain, lane.domain, amount, lane.mintRecipient, nonce, hookData);
        emit FinalityPolicyUsed(destination.chain, lane.domain, FINALITY_THRESHOLD_FINALIZED);
        uint256 allowanceBefore = IERC20(USDC).allowance(address(this), address(TOKEN_MESSENGER));
        IERC20(USDC).forceApprove(address(TOKEN_MESSENGER), amount);
        if (IERC20(USDC).allowance(address(this), address(TOKEN_MESSENGER)) != amount) {
            revert InvalidAmount();
        }
        TOKEN_MESSENGER.depositForBurnWithHook(
            amount,
            lane.domain,
            lane.mintRecipient,
            USDC,
            // Left open on purpose. Naming a destination caller would mean a transfer only
            // completes when Hyperion's own key is awake, and a transfer that depends on its
            // operator being awake is not a transfer anybody should rely on.
            bytes32(0),
            // Nothing, because the finalized tier does not charge a transfer fee. A nonzero value
            // here would be money the sender never agreed to spend.
            0,
            FINALITY_THRESHOLD_FINALIZED,
            hookData
        );
        // Closing the allowance leaves only the documented allowance state (0).
        IERC20(USDC).forceApprove(address(TOKEN_MESSENGER), 0);
        uint256 allowanceAfter = IERC20(USDC).allowance(address(this), address(TOKEN_MESSENGER));
        if (allowanceAfter != DOCUMENTED_RESIDUAL_ALLOWANCE) revert InvalidAmount();

        emit ApprovalAudited(USDC, address(TOKEN_MESSENGER), allowanceBefore, allowanceAfter, true);

        // CCTP wanted no native currency, so none of it is this contract's to keep.
        _refund();

        // Circle does not hand back a handle. The burn is identified by its log on this chain and
        // by the nonce inside the message Circle signs, neither of which is available here, so
        // returning a made up value would be worse than returning none.
        return bytes32(0);
    }

    /// @notice Make a destination reachable.
    /// @dev A lane can be configured once and after that only switched off. Repointing a live
    /// lane's mint recipient would move every future burn on it to a different contract, which is
    /// the same blast radius as swapping the adapter itself, so it is done the same way: deploy
    /// another adapter and let the router's timelock point at it.
    function setLane(string calldata chain, uint32 domain, bytes32 mintRecipient) external onlyOwner {
        if (bytes(chain).length == 0) revert UnknownChain();
        if (mintRecipient == bytes32(0)) revert ZeroAddress();
        Lane storage lane = _lanes[chain];
        if (lane.configured) revert AlreadyConfigured();
        lane.domain = domain;
        lane.mintRecipient = mintRecipient;
        lane.configured = true;
        lane.enabled = true;
        emit LaneConfigured(chain, domain, mintRecipient);
        emit LaneEnabled(chain, true);
    }

    /// @notice Turn a configured destination on or off.
    function setLaneEnabled(string calldata chain, bool enabled) external onlyOwner {
        Lane storage lane = _lanes[chain];
        if (!lane.configured) revert RailNotConfigured();
        lane.enabled = enabled;
        emit LaneEnabled(chain, enabled);
    }

    /// @notice Read a configured destination.
    function laneOf(string calldata chain) external view returns (Lane memory) {
        return _lanes[chain];
    }

    /// @notice Propose a rotation of the peer messenger / mint recipient on a remote chain.
    /// @param chain The Hyperion chain name.
    /// @param newPeer The new peer mint recipient address (bytes32).
    /// @param delay Timelock delay in seconds (must be >= MIN_ROTATION_DELAY).
    function proposePeerRotation(string calldata chain, bytes32 newPeer, uint64 delay) external onlyOwner {
        Lane storage lane = _lanes[chain];
        if (!lane.configured) revert UnknownChain();
        if (newPeer == bytes32(0)) revert ZeroAddress();
        if (newPeer == lane.mintRecipient) revert AlreadyConfigured();
        if (delay < MIN_ROTATION_DELAY) revert TimelockDelayOutOfRange();
        if (_pendingRotations[chain].eta != 0) revert AlreadyConfigured();
        if (block.timestamp > type(uint64).max - delay) revert DecimalOverflow();

        uint64 nowTime = uint64(block.timestamp);
        uint64 eta = nowTime + delay;
        _pendingRotations[chain] = PeerRotation({newMintRecipient: newPeer, eta: eta, queuedAt: nowTime});

        emit PeerRotationQueued(chain, lane.mintRecipient, newPeer, eta);
    }

    /// @notice Apply a previously proposed peer rotation after the delay has passed.
    /// @param chain The Hyperion chain name.
    function applyPeerRotation(string calldata chain) external onlyOwner {
        Lane storage lane = _lanes[chain];
        if (!lane.configured) revert UnknownChain();

        PeerRotation memory rotation = _pendingRotations[chain];
        if (rotation.eta == 0) revert TimelockNotQueued();
        if (block.timestamp < rotation.eta) revert TimelockNotReady(rotation.eta);

        bytes32 oldPeer = lane.mintRecipient;
        lane.mintRecipient = rotation.newMintRecipient;
        delete _pendingRotations[chain];

        uint64 overlapDeadline = uint64(block.timestamp) + DEFAULT_OVERLAP_WINDOW;
        _historicalPeers[chain] = HistoricalPeer({oldMintRecipient: oldPeer, validUntil: overlapDeadline});

        emit PeerRotationApplied(chain, oldPeer, lane.mintRecipient, overlapDeadline);
    }

    /// @notice Cancel a pending peer rotation.
    /// @param chain The Hyperion chain name.
    function cancelPeerRotation(string calldata chain) external onlyOwner {
        Lane storage lane = _lanes[chain];
        if (!lane.configured) revert UnknownChain();

        PeerRotation memory rotation = _pendingRotations[chain];
        if (rotation.eta == 0) revert TimelockNotQueued();

        bytes32 cancelled = rotation.newMintRecipient;
        delete _pendingRotations[chain];

        emit PeerRotationCancelled(chain, cancelled);
    }

    /// @notice Read the pending rotation for a chain.
    function pendingRotationOf(string calldata chain) external view returns (PeerRotation memory) {
        return _pendingRotations[chain];
    }

    /// @notice Read the historical peer window for a chain.
    function historicalPeerOf(string calldata chain) external view returns (HistoricalPeer memory) {
        return _historicalPeers[chain];
    }

    /// @notice Audits current allowance granted to the token messenger for USDC.
    /// @dev Read-only inspection. Unexpected residual approvals produce findings
    /// without automatic mutation.
    /// @return Diagnostics snapshot including allowance, expected allowance, and finding.
    function auditAllowance() external view returns (ApprovalDiagnostics memory) {
        return auditAllowance(USDC);
    }

    /// @notice Audits current allowance granted to the token messenger for a specific token.
    /// @dev Read-only inspection. Unexpected residual approvals produce findings
    /// without automatic mutation.
    /// @param token The token address to audit.
    /// @return Diagnostics snapshot including allowance, expected allowance, and finding.
    function auditAllowance(address token) public view returns (ApprovalDiagnostics memory) {
        (bool readable, bytes memory result) =
            token.staticcall(abi.encodeCall(IERC20.allowance, (address(this), address(TOKEN_MESSENGER))));
        readable = readable && result.length == 32;
        uint256 currentAllowance = readable ? abi.decode(result, (uint256)) : 0;
        bool isClean = readable && currentAllowance == DOCUMENTED_RESIDUAL_ALLOWANCE;
        ApprovalFinding finding = !readable
            ? ApprovalFinding.Unavailable
            : (isClean ? ApprovalFinding.Clean : ApprovalFinding.UnexpectedResidual);

        return ApprovalDiagnostics({
            version: APPROVAL_DIAGNOSTICS_VERSION,
            token: token,
            spender: address(TOKEN_MESSENGER),
            allowance: currentAllowance,
            expectedAllowance: DOCUMENTED_RESIDUAL_ALLOWANCE,
            isClean: isClean,
            finding: finding
        });
    }

    /// @notice Configure the optional transmitter identity for provider pause diagnostics.
    function setMessageTransmitter(address transmitter) external onlyOwner {
        if (transmitter == address(0)) revert ZeroAddress();
        messageTransmitter = transmitter;
        emit MessageTransmitterSet(transmitter);
    }

    /// @notice Coherent lane readiness across router and upstream providers.
    function laneReadiness() external view returns (LaneReadiness memory) {
        bool routerPaused = false;
        (bool routerRead, bytes memory routerResult) = ROUTER.staticcall(abi.encodeWithSignature("paused()"));
        if (routerRead && routerResult.length == 32) {
            routerPaused = abi.decode(routerResult, (bool));
        } else {
            routerPaused = true;
        }

        ProviderReadStatus messenger = _providerPause(address(TOKEN_MESSENGER));
        ProviderReadStatus transmitter = messageTransmitter == address(0)
            ? ProviderReadStatus.Unknown
            : _providerPause(messageTransmitter);
        bool available = !routerPaused && messenger == ProviderReadStatus.Usable
            && transmitter == ProviderReadStatus.Usable;

        return LaneReadiness({
            version: 1,
            routerPaused: routerPaused,
            messenger: messenger,
            transmitter: transmitter,
            messengerContract: address(TOKEN_MESSENGER),
            transmitterContract: messageTransmitter,
            laneAvailable: available
        });
    }

    /// @notice Classify renewal evidence without verification or mint mutation.
    function prepareRenewal(
        bytes32 sourceMessageId,
        bytes32 payoutIntent,
        uint256 expiresAt,
        bool verifiedAttestation,
        bool recoverySupported
    ) external view returns (RenewalPlan memory) {
        AttestationCondition condition = expiresAt <= block.timestamp
            ? AttestationCondition.Expired
            : !recoverySupported
                ? AttestationCondition.UnsupportedRecovery
                : verifiedAttestation ? AttestationCondition.Verified : AttestationCondition.Unverified;
        return RenewalPlan({
            version: 1,
            condition: condition,
            sourceMessageId: sourceMessageId,
            payoutIntent: payoutIntent,
            expiresAt: expiresAt,
            recoverySupported: recoverySupported && expiresAt > block.timestamp
        });
    }

    function _providerPause(address provider) private view returns (ProviderReadStatus) {
        (bool readable, bytes memory result) = provider.staticcall(abi.encodeWithSignature("paused()"));
        if (!readable || result.length != 32) return ProviderReadStatus.Unknown;
        return abi.decode(result, (bool)) ? ProviderReadStatus.Paused : ProviderReadStatus.Usable;
    }

    /// @dev Adapters hold nothing. Anything sent here goes straight back to the router, which
    /// gives it back to whoever sent the transfer.
    function _refund() private {
        uint256 balance = address(this).balance;
        if (balance == 0) return;
        (bool sent,) = payable(ROUTER).call{value: balance}("");
        if (!sent) revert RefundFailed();
    }
}
