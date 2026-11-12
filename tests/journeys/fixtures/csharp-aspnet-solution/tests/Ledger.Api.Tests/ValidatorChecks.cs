using Ledger.Domain;

namespace Ledger.Api.Tests;

public static class ValidatorChecks
{
    public static bool RejectsMissingOwner()
    {
        var errors = AccountValidator.MissingFields(new CreateAccountRequest(null, 10m));
        return errors.ContainsKey("owner");
    }

    public static Account Sample()
    {
        var request = new CreateAccountRequest("sample", 5m);
        return AccountValidator.ToAccount(request);
    }

    public static Account Fixed() => new Account(Guid.Empty, "fixed", 1m);
}
