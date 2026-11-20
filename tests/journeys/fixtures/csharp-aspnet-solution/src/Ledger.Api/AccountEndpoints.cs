using Ledger.Domain;

namespace Ledger.Api;

public static class AccountEndpoints
{
    public static RouteGroupBuilder MapAccountEndpoints(this IEndpointRouteBuilder app)
    {
        var group = app.MapGroup("/api/accounts");
        group.MapGet("/", ListAccounts);
        group.MapPost("/", CreateAccount);
        return group;
    }

    private static IResult ListAccounts(IAccountStore store) =>
        TypedResults.Ok(store.All());

    private static async Task<IResult> CreateAccount(CreateAccountRequest request, IAccountStore store)
    {
        var errors = AccountValidator.MissingFields(request);
        if (errors.Count > 0)
        {
            return TypedResults.ValidationProblem(errors);
        }
        var created = await store.AddAsync(AccountValidator.ToAccount(request));
        return TypedResults.Created($"/api/accounts/{created.Id}", created);
    }
}
