using Microsoft.EntityFrameworkCore;

namespace Pantry.Api.Data;

public static class AppDbContextFactory
{
    public static AppDbContext Create(string connection = "Data Source=pantry.db")
    {
        var options = new DbContextOptionsBuilder<AppDbContext>().UseSqlite(connection).Options;
        return new AppDbContext(options);
    }
}
