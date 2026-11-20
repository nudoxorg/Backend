using Notes;

var builder = WebApplication.CreateBuilder(args);
builder.Services.AddSingleton<NoteStore>();

var app = builder.Build();
app.MapNoteEndpoints();
app.Run();

public partial class Program;
