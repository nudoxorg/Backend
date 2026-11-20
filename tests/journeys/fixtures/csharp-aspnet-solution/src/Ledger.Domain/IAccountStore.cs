namespace Ledger.Domain;

public interface IAccountStore
{
    Task<Account?> FindAsync(Guid id);

    Task<Account> AddAsync(Account account);

    IReadOnlyCollection<Account> All();
}
