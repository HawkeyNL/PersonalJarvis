// Portfolio (holdings) API — all calls are authenticated with the session token.
import { currentSession } from "./auth";
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

async function token(): Promise<string> {
  const session = await currentSession();
  if (!session.token) {
    throw new Error("niet ingelogd");
  }
  return session.token;
}

export async function listHoldings(): Promise<Holdings> {
  return getJsonAuth<Holdings>("/v1/holdings", await token());
}

export async function addHolding(input: {
  symbol: string;
  quantity: string;
  avg_cost: string;
  currency?: string;
}): Promise<void> {
  await postJsonAuth("/v1/holdings", await token(), input);
}

export async function deleteHolding(id: string): Promise<void> {
  await deleteAuth(`/v1/holdings/${id}`, await token());
}
