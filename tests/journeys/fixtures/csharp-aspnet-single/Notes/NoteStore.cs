namespace Notes;

public sealed class NoteStore
{
    private readonly List<Note> notes = [];

    public Note Add(string text)
    {
        var note = new Note(notes.Count + 1, text);
        notes.Add(note);
        return note;
    }

    public IReadOnlyList<Note> All() => notes;
}
