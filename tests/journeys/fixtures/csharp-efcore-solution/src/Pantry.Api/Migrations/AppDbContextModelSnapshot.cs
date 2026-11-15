using Microsoft.EntityFrameworkCore;
using Microsoft.EntityFrameworkCore.Infrastructure;
using Pantry.Api.Data;

#nullable disable

namespace Pantry.Api.Migrations;

[DbContext(typeof(AppDbContext))]
partial class AppDbContextModelSnapshot : ModelSnapshot
{
    protected override void BuildModel(ModelBuilder modelBuilder)
    {
        modelBuilder.HasAnnotation("ProductVersion", "8.0.11");
        modelBuilder.Entity("Pantry.Api.Data.PantryItem", entity =>
        {
            entity.Property<int>("Id").ValueGeneratedOnAdd();
            entity.Property<string>("Name").IsRequired();
            entity.Property<string>("OwnerId").IsRequired();
            entity.Property<int>("Quantity");
            entity.HasKey("Id");
        });
    }
}
