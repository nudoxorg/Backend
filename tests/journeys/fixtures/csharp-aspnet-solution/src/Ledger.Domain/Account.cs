namespace Ledger.Domain;

public record Account(Guid Id, string Owner, decimal Balance);

public record CreateAccountRequest(string? Owner, decimal? OpeningBalance);
