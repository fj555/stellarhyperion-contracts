// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {OwnershipReport} from "../../script/OwnershipReport.sol";
import {Unauthorized} from "../../src/HyperionErrors.sol";
import {OutboundRequest, RouteKind} from "../../src/HyperionTypes.sol";
import {AxelarItsAdapter} from "../../src/adapters/AxelarItsAdapter.sol";
import {CctpAdapter} from "../../src/adapters/CctpAdapter.sol";
import {Fixture} from "../Fixture.sol";
import {MockGasService} from "../mocks/MockGasService.sol";
import {MockIts} from "../mocks/MockIts.sol";
import {MockTokenMessenger} from "../mocks/MockTokenMessenger.sol";
import {Ownable} from "openzeppelin/access/Ownable.sol";

/// @notice Real adapter ownership changes do not replace the router's registered rail authority.
contract AdapterOwnershipHandoverTest is Fixture {
    CctpAdapter internal cctp;
    AxelarItsAdapter internal axelar;
    MockTokenMessenger internal messenger;
    bytes32 internal constant TOKEN_ID = keccak256("hyperion.ownership.usdc");
    uint32 internal constant STELLAR_DOMAIN = 27;

    function setUp() public override {
        super.setUp();
        MockIts its = new MockIts();
        its.setRegistered(TOKEN_ID, address(usdc));
        messenger = new MockTokenMessenger();
        cctp = new CctpAdapter(address(router), address(messenger), address(usdc), admin);
        axelar = new AxelarItsAdapter(address(router), address(its), address(new MockGasService()), admin);
        vm.startPrank(admin);
        cctp.setLane(STELLAR, STELLAR_DOMAIN, C_KEY);
        axelar.setPeer(STELLAR, "stellar", bytes(C_ADDR));
        axelar.linkToken(address(usdc), TOKEN_ID);
        axelar.setDestinationCapability(
            STELLAR, address(usdc), 4, AxelarItsAdapter.DestinationCapability.GmpPayload
        );
        axelar.setDevelopmentGate(true, keccak256("independently validated ownership fixture"));
        vm.stopPrank();
        _setAdapter(RouteKind.Cctp, address(cctp));
        _setAdapter(RouteKind.AxelarIts, address(axelar));
    }

    function test_pending_owner_cannot_configure_either_real_adapter() public {
        _nominateBoth();
        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, alice));
        cctp.setLane("next", STELLAR_DOMAIN, C_KEY);
        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, alice));
        axelar.setPeer("next", "axelar-next", bytes(C_ADDR));
        _assertRouterAuthority();
    }

    function test_acceptance_changes_config_owner_without_changing_dispatch_authority() public {
        _nominateBoth();
        _acceptBoth();
        vm.startPrank(alice);
        cctp.setLane("next", STELLAR_DOMAIN, C_KEY);
        axelar.setPeer("next", "axelar-next", bytes(C_ADDR));
        vm.stopPrank();
        assertTrue(cctp.supportsChain("next"));
        assertTrue(axelar.supportsChain("next"));
        _assertRouterAuthority();

        // Adapter configuration ownership never grants the owner permission to dispatch directly.
        vm.prank(alice);
        vm.expectRevert(Unauthorized.selector);
        cctp.dispatch(address(usdc), 1, _dest(G_ADDR), 1);
        vm.prank(alice);
        vm.expectRevert(Unauthorized.selector);
        axelar.dispatch(address(usdc), 1, _dest(G_ADDR), 1);
    }

    function test_previous_owner_cannot_configure_after_acceptance() public {
        _nominateBoth();
        _acceptBoth();
        vm.prank(admin);
        vm.expectRevert(abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, admin));
        cctp.setLane("next", STELLAR_DOMAIN, C_KEY);
        vm.prank(admin);
        vm.expectRevert(abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, admin));
        axelar.setPeer("next", "axelar-next", bytes(C_ADDR));
        _assertRouterAuthority();
    }

    function test_router_still_dispatches_paid_transfer_after_both_accept() public {
        _nominateBoth();
        _acceptBoth();
        uint256 amount = 1000e6;
        uint256 fee = amount * FEE_BPS / 10_000;
        uint256 previousBalance = usdc.balanceOf(alice);
        OutboundRequest memory request = _request(amount, RouteKind.Cctp);
        request.token = address(usdc);
        vm.prank(alice);
        router.bridgeOut(request);
        assertEq(usdc.balanceOf(alice), previousBalance - amount);
        assertEq(usdc.balanceOf(treasury), fee);
        MockTokenMessenger.Burn memory burn = messenger.lastBurn();
        assertEq(burn.amount, amount - fee);
        assertEq(burn.destinationDomain, STELLAR_DOMAIN);
        assertEq(burn.mintRecipient, C_KEY);
        _assertRouterAuthority();
    }

    function test_repeated_acceptance_cannot_transfer_authority_again() public {
        _nominateBoth();
        _acceptBoth();
        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, alice));
        cctp.acceptOwnership();
        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, alice));
        axelar.acceptOwnership();
        assertEq(cctp.owner(), alice);
        assertEq(axelar.owner(), alice);
        assertEq(cctp.pendingOwner(), address(0));
        assertEq(axelar.pendingOwner(), address(0));
        _assertRouterAuthority();
    }

    function test_report_keeps_one_accepted_and_one_pending_contract_distinct() public {
        _nominateBoth();
        vm.prank(alice);
        cctp.acceptOwnership();
        OwnershipReport.AdapterState memory first = OwnershipReport.adapterState(address(cctp));
        OwnershipReport.AdapterState memory second = OwnershipReport.adapterState(address(axelar));
        assertEq(first.account, address(cctp));
        assertEq(first.owner, alice);
        assertEq(first.pendingOwner, address(0));
        assertEq(second.account, address(axelar));
        assertEq(second.owner, admin);
        assertEq(second.pendingOwner, alice);
        assertEq(first.router, address(router));
        assertEq(second.router, address(router));
        OwnershipReport.RouterState memory actual = OwnershipReport.routerState(router, admin, guardian);
        assertEq(actual.account, address(router));
        assertTrue(actual.expectedAdminActive);
        assertTrue(actual.expectedGuardianActive);
        assertEq(actual.adminRole, router.DEFAULT_ADMIN_ROLE());
        _assertRouterAuthority();
    }

    function _nominateBoth() internal {
        vm.startPrank(admin);
        cctp.transferOwnership(alice);
        axelar.transferOwnership(alice);
        vm.stopPrank();
    }

    function _acceptBoth() internal {
        vm.prank(alice);
        cctp.acceptOwnership();
        vm.prank(alice);
        axelar.acceptOwnership();
    }

    function _assertRouterAuthority() internal view {
        assertEq(cctp.ROUTER(), address(router));
        assertEq(axelar.ROUTER(), address(router));
        assertEq(router.adapter(RouteKind.Cctp), address(cctp));
        assertEq(router.adapter(RouteKind.AxelarIts), address(axelar));
        assertTrue(router.hasRole(router.DEFAULT_ADMIN_ROLE(), admin));
        assertFalse(router.hasRole(router.DEFAULT_ADMIN_ROLE(), alice));
        assertTrue(router.hasRole(router.GUARDIAN_ROLE(), guardian));
    }
}
