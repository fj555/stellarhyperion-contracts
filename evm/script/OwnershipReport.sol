// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {HyperionRouter} from "../src/HyperionRouter.sol";
import {Ownable2Step} from "openzeppelin/access/Ownable2Step.sol";

interface IRouterBoundAdapter {
    function ROUTER() external view returns (address);
}

/// @notice Local tooling reads actual authority; the router uses AccessControl, not Ownable.
library OwnershipReport {
    struct AdapterState {
        address account;
        address owner;
        address pendingOwner;
        address router;
    }

    struct RouterState {
        address account;
        bytes32 adminRole;
        bytes32 guardianRole;
        bool expectedAdminActive;
        bool expectedGuardianActive;
    }

    function adapterState(address account) internal view returns (AdapterState memory) {
        return AdapterState({
            account: account,
            owner: Ownable2Step(account).owner(),
            pendingOwner: Ownable2Step(account).pendingOwner(),
            router: IRouterBoundAdapter(account).ROUTER()
        });
    }

    function routerState(HyperionRouter router, address admin, address guardian)
        internal
        view
        returns (RouterState memory)
    {
        return RouterState({
            account: address(router),
            adminRole: router.DEFAULT_ADMIN_ROLE(),
            guardianRole: router.GUARDIAN_ROLE(),
            expectedAdminActive: router.hasRole(router.DEFAULT_ADMIN_ROLE(), admin),
            expectedGuardianActive: router.hasRole(router.GUARDIAN_ROLE(), guardian)
        });
    }
}
