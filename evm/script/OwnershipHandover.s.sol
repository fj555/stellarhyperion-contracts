// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Script} from "forge-std/Script.sol";
import {stdJson} from "forge-std/StdJson.sol";
import {Ownable2Step} from "openzeppelin/access/Ownable2Step.sol";

import {HyperionRouter} from "../src/HyperionRouter.sol";
import {RouteKind} from "../src/HyperionTypes.sol";
import {OwnershipReport} from "./OwnershipReport.sol";

/// @notice Local-only adapter configuration handover; router roles and registration stay separate.
contract OwnershipHandover is Script {
    using stdJson for string;

    function run() external {
        require(block.chainid == 31_337 || block.chainid == 1337, "local rehearsal only");
        string memory record = vm.readFile(
            vm.envOr(
                "HYPERION_RECORD",
                string.concat("deployments/evm-", vm.toString(block.chainid), "-phase1.json")
            )
        );
        HyperionRouter router = HyperionRouter(payable(record.readAddress(".router")));
        address cctp = record.readAddress(".cctpAdapter");
        address axelar = record.readAddress(".axelarAdapter");
        address previousOwner = record.readAddress(".deployer");
        address nextOwner = vm.envOr("HYPERION_HANDOVER_NEXT_OWNER", address(0));
        string memory mode = vm.envOr("HYPERION_HANDOVER_MODE", string("report"));
        require(cctp != axelar, "distinct adapters required");
        require(router.adapter(RouteKind.Cctp) == cctp, "cctp registration mismatch");
        require(router.adapter(RouteKind.AxelarIts) == axelar, "axelar registration mismatch");
        require(OwnershipReport.adapterState(cctp).router == address(router), "cctp router mismatch");
        require(OwnershipReport.adapterState(axelar).router == address(router), "axelar router mismatch");

        if (keccak256(bytes(mode)) == keccak256("nominate")) {
            require(nextOwner != address(0) && nextOwner != previousOwner, "new nonzero owner required");
            require(msg.sender == previousOwner, "previous owner must nominate");
            vm.startBroadcast();
            _nominate(cctp, previousOwner, nextOwner);
            _nominate(axelar, previousOwner, nextOwner);
            vm.stopBroadcast();
        } else if (keccak256(bytes(mode)) == keccak256("accept")) {
            require(nextOwner != address(0) && nextOwner != previousOwner, "new nonzero owner required");
            require(msg.sender == nextOwner, "pending owner must accept");
            vm.startBroadcast();
            _accept(cctp, previousOwner, nextOwner);
            _accept(axelar, previousOwner, nextOwner);
            vm.stopBroadcast();
        } else {
            require(keccak256(bytes(mode)) == keccak256("report"), "unknown handover mode");
        }

        string memory key = "ownership-handover";
        vm.serializeUint(key, "chainId", block.chainid);
        vm.serializeString(key, "mode", mode);
        vm.serializeUint(key, "observedAtBlock", block.number);
        vm.serializeAddress(key, "router", address(router));
        OwnershipReport.RouterState memory authority = OwnershipReport.routerState(
            router, record.readAddress(".admin"), record.readAddress(".guardian")
        );
        vm.serializeBytes32(key, "routerAdminRole", authority.adminRole);
        vm.serializeBytes32(key, "routerGuardianRole", authority.guardianRole);
        vm.serializeBool(key, "recordedAdminActive", authority.expectedAdminActive);
        vm.serializeBool(key, "recordedGuardianActive", authority.expectedGuardianActive);
        _writeAdapter(key, "cctp", cctp);
        _writeAdapter(key, "axelar", axelar);
        string memory result = vm.serializeAddress(key, "requestedNextOwner", nextOwner);
        vm.writeJson(
            result, string.concat("deployments/ownership-", vm.toString(block.chainid), "-", mode, ".json")
        );
    }

    function _nominate(address account, address previousOwner, address nextOwner) private {
        OwnershipReport.AdapterState memory state = OwnershipReport.adapterState(account);
        if (state.owner == nextOwner && state.pendingOwner == address(0)) return;
        require(state.owner == previousOwner, "unexpected current owner");
        if (state.pendingOwner == nextOwner) return;
        require(state.pendingOwner == address(0), "different nomination already pending");
        Ownable2Step(account).transferOwnership(nextOwner);
    }

    function _accept(address account, address previousOwner, address nextOwner) private {
        OwnershipReport.AdapterState memory state = OwnershipReport.adapterState(account);
        if (state.owner == nextOwner && state.pendingOwner == address(0)) return;
        require(
            state.owner == previousOwner && state.pendingOwner == nextOwner, "expected nomination required"
        );
        Ownable2Step(account).acceptOwnership();
    }

    function _writeAdapter(string memory key, string memory prefix, address account) private {
        OwnershipReport.AdapterState memory state = OwnershipReport.adapterState(account);
        vm.serializeAddress(key, string.concat(prefix, "Adapter"), state.account);
        vm.serializeAddress(key, string.concat(prefix, "Owner"), state.owner);
        vm.serializeAddress(key, string.concat(prefix, "PendingOwner"), state.pendingOwner);
        vm.serializeAddress(key, string.concat(prefix, "Router"), state.router);
    }
}
