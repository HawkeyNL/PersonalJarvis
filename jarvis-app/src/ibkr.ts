// IBKR (read-only): connection status + positions via the authenticated API.
import { currentAuthStatus } from "./auth";
import { getJsonAuth } from "./api";

export type IbkrStatus = {
  reachable: boolean;
  authenticated: boolean;
  connected?: boolean;
  hint?: string;
};

export type IbkrPosition = {
  conid: number;
  symbol: string;
  position: number;
  avg_cost: number;
  mkt_price: number;
  mkt_value: number;
  currency: string;
};

async function requireAuth(): Promise<void> {
  const status = await currentAuthStatus();
  if (!status.authenticated) {
    throw new Error("niet ingelogd");
  }
}

export async function ibkrStatus(): Promise<IbkrStatus> {
  await requireAuth();
  return getJsonAuth<IbkrStatus>("/v1/broker/ibkr/status");
}

export async function ibkrPositions(): Promise<{
  account: string;
  positions: IbkrPosition[];
}> {
  await requireAuth();
  return getJsonAuth("/v1/broker/ibkr/positions");
}
