using System.Collections.Concurrent;
using Ledger.Domain;

namespace Ledger.Api;

public sealed class InMemoryAccountStore : IAccountStore
{
    private readonly ConcurrentDictionary<Guid, Account> accounts = new();

    public Task<Account?> FindAsync(Guid id) =>
        Task.FromResult(accounts.TryGetValue(id, out var account) ? account : null);

    public Task<Account> AddAsync(Account account)
    {
        accounts[account.Id] = account;
        return Task.FromResult(account);
    }

    public IReadOnlyCollection<Account> All() => accounts.Values.ToArray();
}
