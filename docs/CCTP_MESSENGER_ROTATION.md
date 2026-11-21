# CCTP remote-messenger rotation with overlap controls

CCTP remote-messenger and peer messenger replacement uses `propose_peer_rotation` / `apply_peer_rotation` / `cancel_peer_rotation` (or `propose_messenger_rotation` / `apply_messenger_rotation` / `cancel_messenger_rotation` for local TokenMessenger) on Soroban, and `proposePeerRotation` / `applyPeerRotation` / `cancelPeerRotation` on EVM. The minimum timelock delay is 3,600 seconds (1 hour). Proposals do not alter active routing trust until applied at or after the recorded eta. A proposal must be cancelled before a replacement proposal can be submitted.

## Acceptance windows and historical-message handling

Unlike instantaneous cutover models where old-peer messages in flight are rejected at cutover, CCTP peer messenger replacement introduces an explicit historical-message acceptance window (overlap window, default 86,400 seconds / 24 hours).

| Messenger / Lane state     | Inbound: active peer         | Inbound: retired peer (old)  | Inbound: unauthorized peer | Outbound dispatch  |
| -------------------------- | ---------------------------- | ---------------------------- | -------------------------- | ------------------ |
| Enabled, rotation pending  | Accepted with normal checks  | N/A                          | Refused                    | Uses active route  |
| Enabled, overlap window    | Accepted with normal checks  | Accepted (in-flight overlap) | Refused                    | Uses active route  |
| Enabled, overlap expired   | Accepted with normal checks  | Refused (expired)            | Refused                    | Uses active route  |
| Emergency disabled         | Refused                      | Refused                      | Refused                    | Refused            |
| Unlinked / unmapped        | Refused                      | Refused                      | Refused                    | Refused            |

### Inbound custody protection
Messages from unauthorized peers never acquire inbound custody. During normal operation, only the active peer messenger is trusted. During the acceptance window following an applied rotation, in-flight messages signed against the retired peer messenger are accepted so that transfers crossing the network before cutover settle cleanly into their recipient accounts or claims without stranding funds. Once the acceptance window elapses, old messages are rejected with `OldMessengerExpired`.

### Emergency-disable controls
Administrative disable controls (`set_domain_disabled` on Soroban, `setLaneEnabled` on EVM) halt both outbound burns and inbound custody immediately. Applying or cancelling a rotation never re-enables a disabled domain or lane. Emergency disable preserves funds in place while an upstream fault or provider upgrade anomaly is investigated.

## Provider upgrade compatibility & message boundaries
Provider upgrades must adhere to explicit message-boundary guarantees:
- Exact positive integer amounts (`i128` on Soroban, `uint256` on EVM) without overflow, truncation, or floating-point conversions.
- Authenticated rail dispatch: only the authorized router caller may invoke burn dispatch.
- Replay protection: Circle 32-byte nonces are validated on inbound; router replay protection guards prevent double-crediting.
- Recipient-bound claims: mint recipients and hook payloads cannot be mutated in flight. Parked claims in the router remain payable to their recorded recipient regardless of peer rotation or domain disablement.
- Allowance lifecycle: TokenMessenger consumes approvals via `transfer_from`. Dispatch opens the allowance for the exact amount and ledger sequence, then closes it to zero.

## Active deployment gating
The active deployment remains Stellar Testnet outbound ITS/CCTP. EVM contracts, inbound CCTP forwarding, and live peer migrations remain development paths until independently validated under existing testnet and deployment gates. No production activation or deployment is authorized by these APIs or this document.
