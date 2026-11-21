// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {ITokenManager} from "../../src/interfaces/IAxelar.sol";

/// @title A token manager that only answers the one question asked of it
/// @notice Which kind of manager ITS deployed behind a token id, which decides how funds move.
/// @dev Deliberately not a working manager. Nothing in the adapter's transfer paths calls a
/// manager directly; ITS does. This exists so the registration diagnostic can read a kind without
/// a real ITS deployment, and so a test can set the kind to something an operator would not
/// expect.
contract MockTokenManager is ITokenManager {
    uint256 private immutable KIND;

    constructor(uint256 kind_) {
        KIND = kind_;
    }

    function implementationType() external view override returns (uint256) {
        return KIND;
    }
}
