namespace Ledger.Domain;

public static class AccountValidator
{
    public static Dictionary<string, string[]> MissingFields(CreateAccountRequest request)
    {
        var errors = new Dictionary<string, string[]>();
        if (string.IsNullOrWhiteSpace(request.Owner))
        {
            errors["owner"] = ["owner is required"];
        }
        if (request.OpeningBalance is null)
        {
            errors["openingBalance"] = ["openingBalance is required"];
        }
        return errors;
    }

    public static Account ToAccount(CreateAccountRequest request) =>
        new(Guid.NewGuid(), request.Owner!, request.OpeningBalance!.Value);
}
