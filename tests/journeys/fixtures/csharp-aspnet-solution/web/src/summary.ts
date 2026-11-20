import { listAccounts } from "./client";

export async function totalBalance(): Promise<number> {
  const accounts = await listAccounts();
  return accounts.reduce((sum, account) => sum + account.balance, 0);
}
