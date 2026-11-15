using Microsoft.AspNetCore.Identity;
using Microsoft.AspNetCore.RateLimiting;
using Microsoft.EntityFrameworkCore;
using Pantry.Api.Data;
using Pantry.Api.Endpoints;

var builder = WebApplication.CreateBuilder(args);

builder.Services.AddDbContext<AppDbContext>(options =>
    options.UseSqlite(builder.Configuration.GetConnectionString("Pantry") ?? "Data Source=pantry.db"));
builder.Services.AddIdentityApiEndpoints<IdentityUser>().AddEntityFrameworkStores<AppDbContext>();
builder.Services.AddRateLimiter(options => options.AddFixedWindowLimiter("pantry", window =>
{
    window.PermitLimit = 20;
    window.Window = TimeSpan.FromMinutes(1);
}));

var app = builder.Build();
app.UseRateLimiter();

app.MapPantryEndpoints();

app.Run();

public partial class Program
{
}
