using Microsoft.AspNetCore.Mvc.Testing;
using Microsoft.EntityFrameworkCore;
using Pantry.Api.Data;
using Xunit;

namespace Pantry.Api.Tests;

public class PantryEndpointsTests : IClassFixture<WebApplicationFactory<Program>>
{
    private readonly WebApplicationFactory<Program> factory;

    public PantryEndpointsTests(WebApplicationFactory<Program> factory)
    {
        this.factory = factory;
    }

    [Fact]
    public async Task Items_listing_is_reachable()
    {
        var client = factory.CreateClient();
        var response = await client.GetAsync("/pantry/items");
        Assert.True(response.IsSuccessStatusCode);
    }

    [Fact]
    public void Context_creates_its_schema()
    {
        using var context = AppDbContextFactory.Create("Data Source=:memory:");
        Assert.NotNull(context.Model);
    }

    [Fact]
    public void Context_can_be_built_from_options()
    {
        var options = new DbContextOptionsBuilder<AppDbContext>().UseSqlite("Data Source=:memory:").Options;
        using var context = new AppDbContext(options);
        Assert.Empty(context.Items);
    }

    [Fact]
    public void Items_start_empty()
    {
        var item = new PantryItem();
        Assert.Equal(0, item.Quantity);
    }
}
