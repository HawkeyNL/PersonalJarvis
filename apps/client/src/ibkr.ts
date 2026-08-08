// IBKR (read-only): connection status + positions via the authenticated API.
import { currentSession } from "./auth";
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

async function token(): Promise<string> {
  const session = await currentSession();
  if (!session.token) {
    throw new Error("niet ingelogd");
  }
  return session.token;
}

export async function ibkrStatus(): Promise<IbkrStatus> {
  return getJsonAuth<IbkrStatus>("/v1/broker/ibkr/status", await token());
}

export async function ibkrPositions(): Promise<{
  account: string;
  positions: IbkrPosition[];
}> {
  return getJsonAuth("/v1/broker/ibkr/positions", await token());
}
