import { totalBalance } from "./summary";
import { listAccounts } from "./client";

export async function describeLedger(): Promise<string> {
  const accounts = await listAccounts();
  const total = await totalBalance();
  return `${accounts.length} accounts, balance ${total}`;
}
