using Microsoft.EntityFrameworkCore;
using Pantry.Api.Data;

namespace Pantry.Api.Endpoints;

public static class PantryEndpoints
{
    public static WebApplication MapPantryEndpoints(this WebApplication app)
    {
        app.MapGet("/pantry/items", async (AppDbContext db) => await db.Items.ToListAsync())
            .RequireRateLimiting("pantry");

        app.MapPost("/pantry/items", async (AppDbContext db, PantryItem item) =>
        {
            db.Items.Add(item);
            await db.SaveChangesAsync();
            return Results.Created($"/pantry/items/{item.Id}", item);
        });

        return app;
    }
}
