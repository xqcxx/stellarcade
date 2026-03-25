import type { WalletSessionMeta } from "./wallet-session";

// Auth slice
export interface AuthState {
  isAuthenticated: boolean;
  userId?: string | null;
  token?: string | null; // short-lived auth token or session id
  updatedAt?: number;
}

// Wallet slice
export interface WalletState {
  connected: boolean;
  meta?: WalletSessionMeta | null;
  lastSyncedAt?: number;
}

// Feature flags / app flags
export type AppFlags = Record<string, boolean>;

// Complete global state
export interface GlobalState {
  auth: AuthState;
  wallet: WalletState;
  flags: AppFlags;
  /** Transient optimistic UI patches for game actions (not persisted). */
  optimisticPatches: Record<string, unknown>;
}

// Actions
export type GlobalAction =
  | { type: "AUTH_SET"; payload: { userId: string; token: string } }
  | { type: "AUTH_CLEAR" }
  | { type: "WALLET_SET"; payload: { meta: WalletSessionMeta } }
  | { type: "WALLET_CLEAR" }
  | { type: "FLAGS_SET"; payload: { key: string; value: boolean } }
  | { type: "FLAGS_CLEAR"; payload: { key: string } }
  | { type: "OPTIMISTIC_PATCH"; payload: { key: string; value: unknown } }
  | { type: "OPTIMISTIC_REVERT"; payload: { key: string } }
  | { type: "OPTIMISTIC_CLEAR" }
  | { type: "RESET_ALL" };

// Domain errors
export class GlobalStateError extends Error {
  public code: string;
  constructor(code: string, message?: string) {
    super(message ?? code);
    this.code = code;
    this.name = "GlobalStateError";
  }
}

export class ValidationError extends GlobalStateError {
  constructor(message?: string) {
    super("validation_error", message);
  }
}

export class PreconditionError extends GlobalStateError {
  constructor(message?: string) {
    super("precondition_failed", message);
  }
}
