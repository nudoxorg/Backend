using Ledger.Api;
using Ledger.Domain;

var builder = WebApplication.CreateBuilder(args);
builder.Services.AddSingleton<IAccountStore, InMemoryAccountStore>();

var app = builder.Build();
app.MapAccountEndpoints();
app.MapGet("/health", () => Results.Ok("ok"));
app.Run();

public partial class Program;
