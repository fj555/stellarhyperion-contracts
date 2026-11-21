/**
 * Reading a token registration diagnostic.
 *
 * Both chains answer the same question before an activation — "which fields would disagree if I
 * registered this asset under this token id" — and both return the same list of findings in the
 * same order. This module is the vocabulary and the copy for that list, so an app that shows an
 * operator what a rehearsal found does not have to invent a second set of names.
 *
 * The values match `hyperion_core::axelar::TokenMappingConflict` variant for variant, except that
 * the Rust enum has no `None` and the Solidity one needs one for a zero value. `None` is therefore
 * always index zero and never appears in a report the contracts produce.
 */
import type { Hex } from "viem";

/**
 * One way a proposed registration disagrees with what is already there. Index aligned with the
 * Solidity `TokenMappingConflict` enum, because these values travel in structs an app reads over
 * `eth_call` and a reordering would relabel every diagnostic on the screen.
 */
export const TokenMappingConflict = {
  None: 0,
  TokenId: 1,
  LocalToken: 2,
  Decimals: 3,
  ManagerType: 4,
  UnreadableMetadata: 5,
  UnregisteredTokenId: 6,
  RailTokenMismatch: 7,
} as const;

export type TokenMappingConflict = (typeof TokenMappingConflict)[keyof typeof TokenMappingConflict];

/** Whose problem a finding is, which decides whether an operator or a deploy script fixes it. */
export type ConflictOwner = "local" | "rail" | "asset";

export interface ConflictCopy {
  /** Short enough for a row in a diagnostic list. */
  readonly label: string;
  /** One sentence, written for somebody who did not read the architecture document. */
  readonly detail: string;
  readonly owner: ConflictOwner;
  /** Whether the same call could succeed later without changing anything. */
  readonly transient: boolean;
}

const COPY: Record<TokenMappingConflict, ConflictCopy> = {
  [TokenMappingConflict.None]: {
    label: "Compatible",
    detail: "Nothing disagrees, so this registration can be activated as it stands.",
    owner: "local",
    transient: false,
  },
  [TokenMappingConflict.TokenId]: {
    label: "Asset already linked elsewhere",
    detail:
      "This asset is already registered under a different token id here. Registering it again would change where its transfers go, so it is refused rather than applied.",
    owner: "local",
    transient: false,
  },
  [TokenMappingConflict.LocalToken]: {
    label: "Token id already linked elsewhere",
    detail:
      "This token id is already registered to a different asset here. Repointing it would change which asset an arriving delivery is paid in.",
    owner: "local",
    transient: false,
  },
  [TokenMappingConflict.Decimals]: {
    label: "Decimals disagree",
    detail:
      "The asset reports a different number of decimal places than the registration expects. Getting this wrong is a factor of ten rather than a revert.",
    owner: "asset",
    transient: false,
  },
  [TokenMappingConflict.ManagerType]: {
    label: "Token manager kind disagrees",
    detail:
      "Axelar takes funds for this token id through a different kind of token manager than expected, which changes what an outbound transfer has to authorise.",
    owner: "rail",
    transient: false,
  },
  [TokenMappingConflict.UnreadableMetadata]: {
    label: "Asset does not answer",
    detail:
      "The asset does not implement readable metadata, so its decimals cannot be checked. This is usually an address from the wrong column rather than a broken token.",
    owner: "asset",
    transient: false,
  },
  [TokenMappingConflict.UnregisteredTokenId]: {
    label: "Unknown to Axelar",
    detail:
      "Axelar has no token manager behind this token id on this chain, so there is nothing to route through and nothing for an inbound delivery to trust.",
    owner: "rail",
    transient: false,
  },
  [TokenMappingConflict.RailTokenMismatch]: {
    label: "Token id belongs to another asset",
    detail:
      "Axelar's own registry says this token id is backed by a different asset on this chain. Both halves exist; the pairing is the mistake.",
    owner: "rail",
    transient: false,
  },
};

/**
 * The copy for a finding this build has no name for.
 *
 * A contract can be upgraded ahead of the app that reads it, and a diagnostic is the last place
 * that should throw. Saying the finding is unrecognised is honest and still tells an operator
 * that the registration was refused; showing the `None` copy would claim it was fine.
 */
const UNRECOGNISED: ConflictCopy = {
  label: "Unrecognised finding",
  detail:
    "The contract reported a conflict this build has no name for, which usually means it was upgraded ahead of this app. The registration is still refused; read the contract's own documentation for the newer finding.",
  owner: "local",
  transient: false,
};

/**
 * The copy for a finding.
 *
 * Takes a plain number rather than the union, because a discriminant read off a chain is not
 * constrained to the values this build knows.
 */
export function describeConflict(conflict: number): ConflictCopy {
  const copy = (COPY as Record<number, ConflictCopy | undefined>)[conflict];
  return copy ?? UNRECOGNISED;
}

/**
 * What a registration check observed, whether or not it found a problem.
 *
 * Field for field what both contracts return. `mappedTokenId` and `mappedToken` are zero when
 * nothing is registered in that direction, and `railToken` and `railManager` are zero when Axelar
 * does not know the id.
 */
export interface TokenMappingReport {
  readonly token: Hex;
  readonly tokenId: Hex;
  readonly mappedTokenId: Hex;
  readonly mappedToken: Hex;
  readonly railToken: Hex;
  readonly railManager: Hex;
  /** Axelar's `TokenManagerType` discriminant. Zero is a real kind, not an absence. */
  readonly railManagerType: number;
  /**
   * Whether the manager answered at all.
   *
   * Zero is a real kind, so the discriminant alone cannot distinguish a native interchain token
   * manager from a manager this build could not read.
   */
  readonly managerTypeKnown: boolean;
  readonly onchainDecimals: number;
  /**
   * Whether the asset answered `decimals()` at all.
   *
   * Soroban returns `null` here where EVM returns `false`, because a SEP-41 asset that does not
   * implement metadata and an ERC20 that reverts are the same finding expressed by two different
   * type systems.
   */
  readonly decimalsKnown: boolean;
  readonly conflicts: readonly TokenMappingConflict[];
}

/** Whether a report describes a registration that can be activated as it stands. */
export function isCompatible(report: TokenMappingReport): boolean {
  return report.conflicts.length === 0;
}

/** The copy for every finding in a report, in the order the contracts reported them. */
export function explainReport(report: TokenMappingReport): ConflictCopy[] {
  return report.conflicts.map(describeConflict);
}
