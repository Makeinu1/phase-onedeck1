import type { FormatConfig, MatchType } from "../adapter/types";

export const WS_SESSION_STORAGE_KEY = "phase-ws-session";
export const WS_SESSION_TTL_MS = 2 * 60 * 60 * 1000;

export interface WsHostSessionData {
  formatConfig: FormatConfig;
  timerSeconds: number | null;
  matchType: MatchType;
}

/** Exact server-issued identity for a resumable Full session. */
export interface FullSessionKey {
  game_code: string;
  generation: number;
}

export interface WsSessionData {
  gameCode: string;
  playerToken: string;
  fullKey: FullSessionKey;
  serverUrl: string;
  timestamp: number;
  /** Pending terminal ACK routing must outlive playable reconnect expiry. */
  terminalOnly?: true;
  hostSession?: WsHostSessionData;
  hostIsPublic?: boolean;
}

export function isWsSessionValid(session: WsSessionData): boolean {
  return session.terminalOnly === true
    || Date.now() - (session.timestamp ?? 0) < WS_SESSION_TTL_MS;
}

export function loadWsSession(): WsSessionData | null {
  try {
    const raw = localStorage.getItem(WS_SESSION_STORAGE_KEY);
    if (!raw) return null;

    const session = JSON.parse(raw) as WsSessionData;
    if (
      !isWsSessionValid(session)
      || !session.fullKey
      || session.fullKey.game_code !== session.gameCode
      || !Number.isInteger(session.fullKey.generation)
      || session.fullKey.generation < 1
    ) {
      clearWsSession();
      return null;
    }
    return session;
  } catch {
    clearWsSession();
    return null;
  }
}

type WsSessionIdentity = Pick<WsSessionData, "fullKey" | "serverUrl" | "playerToken">;

function matchesStoredWsSession(expected: WsSessionIdentity): boolean {
  const current = loadWsSession();
  return current !== null
    && current.serverUrl === expected.serverUrl
    && current.fullKey.game_code === expected.fullKey.game_code
    && current.fullKey.generation === expected.fullKey.generation
    && current.playerToken === expected.playerToken;
}

export function saveWsSession(session: WsSessionData, expected?: WsSessionIdentity): void {
  try {
    if (expected && !matchesStoredWsSession(expected)) return;
    localStorage.setItem(WS_SESSION_STORAGE_KEY, JSON.stringify(session));
  } catch {
    // A blocked/quota-limited store disables reconnect persistence, not hosting.
  }
}

export function clearWsSession(expected?: WsSessionIdentity): void {
  try {
    if (expected && !matchesStoredWsSession(expected)) return;
    localStorage.removeItem(WS_SESSION_STORAGE_KEY);
  } catch {
    // Nothing else can be done if the browser refuses storage access.
  }
}
