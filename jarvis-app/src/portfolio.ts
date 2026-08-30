// Portfolio calls are authenticated by the native layer; Vue never receives
// the session bearer.
import { currentAuthStatus } from "./auth";
import { deleteAuth, getJsonAuth, postJsonAuth } from "./api";

export type Holding = {
  id: string;
  symbol: string;
  quantity: string;
  avg_cost: string;
  currency: string;
  cost_basis: string;
  weight_pct: string;
};

export type Holdings = {
  holdings: Holding[];
  total_cost: string;
};

async function requireAuth(): Promise<void> {
  const status = await currentAuthStatus();
  if (!status.authenticated) {
    throw new Error("niet ingelogd");
  }
}

export async function listHoldings(): Promise<Holdings> {
  await requireAuth();
  return getJsonAuth<Holdings>("/v1/holdings");
}

export async function addHolding(input: {
  symbol: string;
  quantity: string;
  avg_cost: string;
  currency?: string;
}): Promise<void> {
  await requireAuth();
  await postJsonAuth("/v1/holdings", input);
}

export async function deleteHolding(id: string): Promise<void> {
  await requireAuth();
  await deleteAuth(`/v1/holdings/${id}`);
}
