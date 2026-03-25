import type {
  GlobalState,
  GlobalAction,
  AuthState,
  WalletState,
} from "../types/global-state";
import { ValidationError } from "../types/global-state";
import type { WalletSessionMeta } from "../types/wallet-session";

type Subscriber = (state: GlobalState) => void;

const DEFAULT_KEY = "stc_global_state_v1";

const initialState: GlobalState = {
  auth: { isAuthenticated: false },
  wallet: { connected: false },
  flags: {},
  optimisticPatches: {},
};

export class GlobalStateStore {
  private state: GlobalState;
  private subscribers: Set<Subscriber> = new Set();
  private storageKey: string;

  constructor(opts?: { storageKey?: string }) {
    this.storageKey = opts?.storageKey ?? DEFAULT_KEY;
    this.state = this.restore() ?? initialState;
  }

  // Subscribe to state changes
  subscribe(fn: Subscriber) {
    this.subscribers.add(fn);
    fn(this.state);
    return () => {
      this.subscribers.delete(fn);
    };
  }

  private notify() {
    for (const s of this.subscribers) {
      try {
        s(this.state);
      } catch (e) {
        // swallow subscriber errors
      }
    }
  }

  // Deterministic reducer
  private reducer(state: GlobalState, action: GlobalAction): GlobalState {
    switch (action.type) {
      case "AUTH_SET":
        return {
          ...state,
          auth: {
            isAuthenticated: true,
            userId: action.payload.userId,
            token: action.payload.token,
            updatedAt: Date.now(),
          },
        };
      case "AUTH_CLEAR":
        return { ...state, auth: { isAuthenticated: false } };
      case "WALLET_SET":
        return {
          ...state,
          wallet: {
            connected: true,
            meta: action.payload.meta,
            lastSyncedAt: Date.now(),
          },
        };
      case "WALLET_CLEAR":
        return { ...state, wallet: { connected: false } };
      case "FLAGS_SET":
        return {
          ...state,
          flags: { ...state.flags, [action.payload.key]: action.payload.value },
        };
      case "FLAGS_CLEAR": {
        const { [action.payload.key]: _removed, ...rest } = state.flags;
        return { ...state, flags: rest };
      }
      case "OPTIMISTIC_PATCH":
        return {
          ...state,
          optimisticPatches: {
            ...state.optimisticPatches,
            [action.payload.key]: action.payload.value,
          },
        };
      case "OPTIMISTIC_REVERT": {
        const { [action.payload.key]: _r, ...rest } = state.optimisticPatches;
        return { ...state, optimisticPatches: rest };
      }
      case "OPTIMISTIC_CLEAR":
        return { ...state, optimisticPatches: {} };
      case "RESET_ALL":
        return initialState;
      default:
        return state;
    }
  }

  // Dispatch an action; returns new state. Validations performed here.
  public dispatch(action: GlobalAction): GlobalState {
    // validation examples
    if (action.type === "AUTH_SET") {
      if (!action.payload.userId || !action.payload.token)
        throw new ValidationError("userId and token required");
    }
    if (action.type === "WALLET_SET") {
      if (
        !action.payload.meta ||
        !(action.payload.meta as WalletSessionMeta).address
      )
        throw new ValidationError("wallet meta.address required");
    }

    const next = this.reducer(this.state, action);
    this.state = next;
    // persist durable parts only (auth and flags). wallet considered ephemeral.
    this.persist();
    this.notify();
    return this.state;
  }

  public getState(): GlobalState {
    return this.state;
  }

  // selectors
  public selectAuth(): AuthState {
    return this.state.auth;
  }
  public selectWallet(): WalletState {
    return this.state.wallet;
  }
  public selectFlag(key: string): boolean | undefined {
    return this.state.flags[key];
  }

  private persist() {
    try {
      const payload = {
        auth: this.state.auth,
        flags: this.state.flags,
        storedAt: Date.now(),
      };
      // optimisticPatches intentionally not persisted
      localStorage.setItem(this.storageKey, JSON.stringify(payload));
    } catch (e) {
      // ignore persistence errors
    }
  }

  private restore(): GlobalState | null {
    try {
      const raw = localStorage.getItem(this.storageKey);
      if (!raw) return null;
      const parsed = JSON.parse(raw) as {
        auth?: AuthState;
        flags?: Record<string, boolean>;
      };
      return {
        ...initialState,
        auth: parsed.auth ?? initialState.auth,
        flags: parsed.flags ?? initialState.flags,
      };
    } catch (e) {
      return null;
    }
  }
}

export default GlobalStateStore;
