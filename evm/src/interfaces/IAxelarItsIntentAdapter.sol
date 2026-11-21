// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Destination} from "../HyperionTypes.sol";

/// @title Sender-bound router dispatch for Axelar top-up intents
/// @notice Optional router-to-Axelar entry point that carries the original user's address.
interface IAxelarItsIntentAdapter {
    /// @notice Dispatch a transfer with its authenticated original sender.
    /// @param token The canonical local ERC20.
    /// @param amount Exact token amount already held by the adapter.
    /// @param destination Immutable destination chain and recipient.
    /// @param nonce Router outbound transfer nonce.
    /// @param sender Original sender authenticated by the router.
    /// @return railRef Provider reference for the submitted transfer.
    function dispatchWithSender(
        address token,
        uint256 amount,
        Destination calldata destination,
        uint64 nonce,
        address sender
    ) external payable returns (bytes32 railRef);
}
