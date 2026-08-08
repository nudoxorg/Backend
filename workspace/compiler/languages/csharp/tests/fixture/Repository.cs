using System.ComponentModel;

namespace Nudox.Fixture;

/// <summary>
/// Stores <typeparamref name="T"/> entries and hands them back by key.
/// </summary>
/// <typeparam name="T">
/// the stored entry type; must be a reference type with a parameterless
/// constructor so the repository can materialise a default
/// </typeparam>
/// <remarks>
/// See <see cref="IReadOnlyCatalog{T}"/> for the covariant read-only view.
/// </remarks>
[Tracked("catalog", Eager = true)]
public class Repository<T> : IReadOnlyCatalog<T>, IDisposable
    where T : class, IEntry, new()
{
    private readonly Dictionary<string, T> _entries = [];
    private bool _disposed;

    /// <summary>Raised after an entry is stored.</summary>
    public event EventHandler<T>? Stored;

    /// <summary>Raised when the repository is cleared. Handlers run inline.</summary>
    protected internal event Action? Cleared;

    /// <inheritdoc/>
    public int Count => _entries.Count;

    /// <inheritdoc/>
    public IEnumerable<T> Entries => _entries.Values;

    /// <summary>The behaviours this repository was built with.</summary>
    /// <value>Defaults to <see cref="CatalogOptions.None"/>.</value>
    public CatalogOptions Options { get; init; }

    /// <summary>The most recent failure, if any.</summary>
    public string? LastError { get; private set; }

    /// <summary>Looks an entry up by key.</summary>
    /// <param name="key">the key to look up</param>
    /// <returns>the stored entry</returns>
    /// <exception cref="BrokenEntryException">when no entry has that key</exception>
    public T this[string key] =>
        _entries.TryGetValue(key, out var entry) ? entry : throw new BrokenEntryException(key);

    /// <summary>Stores an entry, replacing any entry with the same key.</summary>
    /// <param name="entry">the entry to store</param>
    /// <returns><see langword="true"/> when an existing entry was replaced</returns>
    public bool Store(T entry)
    {
        var replaced = _entries.ContainsKey(entry.Key);
        _entries[entry.Key] = entry;
        Stored?.Invoke(this, entry);
        return replaced;
    }

    /// <summary>Loads an entry, waiting for slow storage.</summary>
    /// <param name="key">the key to load</param>
    /// <param name="cancellationToken">cancels the load</param>
    /// <returns>the loaded entry</returns>
    /// <exception cref="BrokenEntryException">when the entry cannot be read</exception>
    public async Task<T> LoadAsync(string key, CancellationToken cancellationToken = default)
    {
        await Task.Yield();
        cancellationToken.ThrowIfCancellationRequested();
        return this[key];
    }

    /// <summary>Streams every key, newest first.</summary>
    /// <returns>the keys, lazily</returns>
    public IEnumerable<string> Keys()
    {
        foreach (var key in _entries.Keys)
        {
            yield return key;
        }
    }

    /// <summary>Tries to take an entry out of the repository.</summary>
    /// <param name="key">the key to remove</param>
    /// <param name="entry">receives the removed entry</param>
    /// <returns><see langword="true"/> when an entry was removed</returns>
    public bool TryTake(string key, out T? entry) => _entries.Remove(key, out entry);

    /// <summary>Stores several entries at once.</summary>
    /// <param name="entries">the entries to store</param>
    /// <returns>the number stored</returns>
    public int StoreAll(params T[] entries)
    {
        foreach (var entry in entries)
        {
            Store(entry);
        }

        return entries.Length;
    }

    /// <summary>Describes the repository as a labelled pair.</summary>
    /// <returns>the count and the option set</returns>
    public (int count, CatalogOptions options) Describe() => (Count, Options);

    /// <summary>Merges another repository in.</summary>
    /// <param name="other">the repository to merge; its entries win</param>
    /// <param name="mode">how to treat duplicates</param>
    public void Merge(in Repository<T> other, Severity mode = Severity.Warning)
    {
        foreach (var entry in other.Entries)
        {
            if (mode != Severity.Error || !_entries.ContainsKey(entry.Key))
            {
                Store(entry);
            }
        }
    }

    /// <summary>Combines two repositories into a new one.</summary>
    /// <param name="left">the first repository</param>
    /// <param name="right">the second repository; its entries win</param>
    /// <returns>a repository holding both</returns>
    public static Repository<T> operator +(Repository<T> left, Repository<T> right)
    {
        var merged = new Repository<T> { Options = left.Options };
        merged.Merge(left);
        merged.Merge(right);
        return merged;
    }

    /// <summary>Reads the repository as its entry count.</summary>
    /// <param name="repository">the repository to measure</param>
    public static implicit operator int(Repository<T> repository) => repository.Count;

    /// <summary>Not part of the supported surface.</summary>
    [EditorBrowsable(EditorBrowsableState.Never)]
    public void ResetInternalState() => _entries.Clear();

    /// <summary>Use <see cref="Store"/> instead.</summary>
    /// <param name="entry">the entry to add</param>
    [Obsolete("Use Store; Add does not raise Stored.", error: false)]
    public void Add(T entry) => _entries[entry.Key] = entry;

    /// <summary>Releases the repository. Callable only through the interface.</summary>
    void IDisposable.Dispose()
    {
        if (_disposed)
        {
            return;
        }

        _disposed = true;
        _entries.Clear();
        Cleared?.Invoke();
    }

    /// <summary>A cursor over a repository's keys.</summary>
    /// <remarks>Nested inside a generic type, so its owner carries type arguments.</remarks>
    public struct Cursor
    {
        /// <summary>The zero-based position.</summary>
        public int Position { get; set; }
    }
}
