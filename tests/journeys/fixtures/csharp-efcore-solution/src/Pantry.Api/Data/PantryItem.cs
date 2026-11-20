namespace Pantry.Api.Data;

public class PantryItem
{
    public int Id { get; set; }
    public string OwnerId { get; set; } = "";
    public string Name { get; set; } = "";
    public int Quantity { get; set; }
}
