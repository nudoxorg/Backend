export interface AccountDto {
  id: string;
  owner: string;
  balance: number;
}

export async function listAccounts(): Promise<AccountDto[]> {
  const response = await fetch("/api/accounts");
  return (await response.json()) as AccountDto[];
}
