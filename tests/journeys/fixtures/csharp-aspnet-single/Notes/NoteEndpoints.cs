namespace Notes;

public static class NoteEndpoints
{
    public static void MapNoteEndpoints(this IEndpointRouteBuilder app)
    {
        app.MapGet("/notes", (NoteStore store) => store.All());
        app.MapPost("/notes", (string text, NoteStore store) => store.Add(text));
    }
}
