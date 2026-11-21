import { chain, isChainKey } from "../chains.js";
import { tryParseStellarAddress } from "../addresses.js";
import { MAX_DECIMALS, MAX_FEE_BPS } from "../amounts.js";
import { ROUTE_KINDS, type RouteKind } from "../routes.js";
import {
  bestQuote,
  planQuotes,
  type QuoteRequest,
  type RouterSnapshot,
  type RouteQuote,
} from "../quotes.js";

/** IDs are concrete source/destination token contracts (Stellar SAC or EVM address), never tickers. */
export interface SnapshotRepresentation {
  readonly tokenDecimals: number;
  readonly destinationNetwork: string;
  readonly destinationTokenId: string;
  readonly destinationDecimals: number;
}
export interface DatedSnapshot {
  readonly id: string;
  readonly takenAt: number;
  readonly network: string;
  readonly tokenId: string;
  /** Required for comparison; older unqualified inputs are explicitly incompatible. */
  readonly representation?: SnapshotRepresentation | undefined;
  readonly snapshot: RouterSnapshot;
}
export const SnapshotStatus = {
  Fresh: "fresh",
  Stale: "stale",
  Incompatible: "incompatible",
  Unavailable: "unavailable",
  Invalid: "invalid",
  Future: "future",
} as const;
export type SnapshotStatus = (typeof SnapshotStatus)[keyof typeof SnapshotStatus];
export interface SnapshotComparison {
  readonly snapshotId: string;
  readonly status: SnapshotStatus;
  readonly quote: RouteQuote | null;
  /** All eligible rails in deterministic enum order, even when a single rail was requested. */
  readonly quotes: readonly RouteQuote[];
  readonly reason: string | null;
  /** Comparing supplied state is read-only evidence, never permission to submit a transfer. */
  readonly planningOnly: true;
}
export interface SnapshotReference {
  readonly network: string;
  readonly tokenId: string;
  readonly now: number;
  readonly representation?: SnapshotRepresentation | undefined;
}
function timestamp(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}
function decimals(value: unknown): value is number {
  return (
    typeof value === "number" && Number.isInteger(value) && value >= 0 && value <= MAX_DECIMALS
  );
}
function tokenKey(network: unknown, id: unknown): string | null {
  if (typeof network !== "string" || !isChainKey(network) || typeof id !== "string") return null;
  if (chain(network).family === "evm") {
    return /^0x[0-9a-fA-F]{40}$/.test(id) && !/^0x0{40}$/.test(id) ? id.toLowerCase() : null;
  }
  const parsed = tryParseStellarAddress(id);
  return parsed.ok && id.startsWith("C") ? id : null;
}
function record(value: unknown): Record<string, unknown> | null {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}
function identity(network: unknown, id: unknown, value: unknown): string | null {
  const representation = record(value);
  if (
    typeof network !== "string" ||
    typeof id !== "string" ||
    representation === null ||
    typeof representation.destinationNetwork !== "string" ||
    !decimals(representation.tokenDecimals) ||
    !decimals(representation.destinationDecimals)
  )
    return null;
  const source = tokenKey(network, id);
  const destination = tokenKey(
    representation.destinationNetwork,
    representation.destinationTokenId,
  );
  if (
    source === null ||
    destination === null ||
    !isChainKey(network) ||
    !isChainKey(representation.destinationNetwork)
  )
    return null;
  // The existing planner accepts Stellar recipients and models EVM departure arithmetic.
  if (
    chain(network).family !== "evm" ||
    chain(representation.destinationNetwork).family !== "stellar" ||
    chain(network).network !== chain(representation.destinationNetwork).network
  )
    return null;
  return JSON.stringify([
    network,
    source,
    representation.tokenDecimals,
    representation.destinationNetwork,
    destination,
    representation.destinationDecimals,
  ]);
}
function validSnapshot(value: unknown): value is RouterSnapshot {
  const state = record(value);
  if (
    state === null ||
    typeof state.paused !== "boolean" ||
    typeof state.tokenRegistered !== "boolean" ||
    typeof state.tokenEnabled !== "boolean" ||
    !decimals(state.tokenDecimals) ||
    typeof state.feeBps !== "number" ||
    !Number.isSafeInteger(state.feeBps) ||
    state.feeBps < 0 ||
    state.feeBps > MAX_FEE_BPS
  )
    return false;
  const flags = [
    record(state.routeEnabled),
    record(state.adapterSet),
    record(state.chainSupported),
  ];
  const flow = record(state.flowAvailable);
  if (flow === null || flags.some((map) => map === null)) return false;
  return ROUTE_KINDS.every((route) => {
    const available = flow[route];
    return (
      flags.every(
        (map) => map !== null && (map[route] === undefined || typeof map[route] === "boolean"),
      ) &&
      (available === undefined || (typeof available === "bigint" && available >= 0n))
    );
  });
}

/** Oldest snapshot first, then ID; winners use bestQuote over fixed enum order, never input order. */
export function compareSnapshots(
  request: Omit<QuoteRequest, "route"> & { readonly route?: RouteKind },
  snapshots: readonly DatedSnapshot[],
  reference: SnapshotReference,
  maxAgeSeconds: number,
): readonly SnapshotComparison[] {
  if (!timestamp(reference.now) || !timestamp(maxAgeSeconds)) {
    throw new RangeError("now and maxAgeSeconds must be non-negative safe integer seconds");
  }
  if (
    typeof request.amount !== "bigint" ||
    !decimals(request.destinationDecimals) ||
    typeof request.strkey !== "string" ||
    (request.route !== undefined && !ROUTE_KINDS.includes(request.route))
  ) {
    throw new TypeError("invalid snapshot quote request");
  }
  const expected = identity(reference.network, reference.tokenId, reference.representation);
  // Untrusted network observations are checked as unknown before entering the typed planner.
  const entries = snapshots.map((value) => record(value));
  const counts = new Map<string, number>();
  for (const entry of entries) {
    if (entry !== null && typeof entry.id === "string")
      counts.set(entry.id, (counts.get(entry.id) ?? 0) + 1);
  }
  const ordered = [...entries].sort((a, b) => {
    const at = timestamp(a?.takenAt) ? a.takenAt : Number.MAX_SAFE_INTEGER;
    const bt = timestamp(b?.takenAt) ? b.takenAt : Number.MAX_SAFE_INTEGER;
    const ai = typeof a?.id === "string" ? a.id : "";
    const bi = typeof b?.id === "string" ? b.id : "";
    return at !== bt ? at - bt : ai < bi ? -1 : ai > bi ? 1 : 0;
  });
  return ordered.map((entry): SnapshotComparison => {
    const snapshotId = typeof entry?.id === "string" ? entry.id : "";
    const unavailable = (status: SnapshotStatus, reason: string): SnapshotComparison => ({
      snapshotId,
      status,
      quote: null,
      quotes: [],
      reason,
      planningOnly: true,
    });
    if (
      entry === null ||
      snapshotId.trim() === "" ||
      counts.get(snapshotId) !== 1 ||
      !timestamp(entry.takenAt)
    ) {
      return unavailable(SnapshotStatus.Invalid, "invalid timestamp, ID or duplicate ID");
    }
    if (entry.takenAt > reference.now)
      return unavailable(SnapshotStatus.Future, "observation is in the future");
    const actual = identity(entry.network, entry.tokenId, entry.representation);
    const representation = record(entry.representation);
    if (
      expected === null ||
      actual === null ||
      actual !== expected ||
      request.destinationDecimals !== representation?.destinationDecimals
    ) {
      return unavailable(
        SnapshotStatus.Incompatible,
        "network or concrete token representation differs or is unqualified",
      );
    }
    if (!validSnapshot(entry.snapshot))
      return unavailable(SnapshotStatus.Invalid, "malformed router state");
    if (entry.snapshot.tokenDecimals !== representation.tokenDecimals) {
      return unavailable(
        SnapshotStatus.Incompatible,
        "snapshot token precision differs from its representation",
      );
    }
    if (reference.now - entry.takenAt > maxAgeSeconds)
      return unavailable(SnapshotStatus.Stale, "observation exceeds maximum age");
    const quotes = planQuotes(request, entry.snapshot);
    const quote = bestQuote(
      request.route === undefined ? quotes : quotes.filter((q) => q.route === request.route),
    );
    return {
      snapshotId,
      status: quote === null ? SnapshotStatus.Unavailable : SnapshotStatus.Fresh,
      quote,
      quotes,
      reason: quote === null ? "no available candidate rail" : null,
      planningOnly: true,
    };
  });
}
