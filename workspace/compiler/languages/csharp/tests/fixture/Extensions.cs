namespace Nudox.Fixture;

/// <summary>Convenience operations over a catalogue.</summary>
public static class CatalogExtensions
{
    /// <summary>Counts the entries at or above a severity.</summary>
    /// <typeparam name="T">the entry type</typeparam>
    /// <param name="catalog">the catalogue to scan</param>
    /// <param name="floor">the lowest severity that counts</param>
    /// <returns>the number of matching entries</returns>
    /// <example>
    /// <code>
    /// var serious = catalog.CountAtLeast(Severity.Warning);
    /// </code>
    /// </example>
    public static int CountAtLeast<T>(this IReadOnlyCatalog<T> catalog, Severity floor)
        where T : class, IEntry
    {
        var total = 0;
        foreach (var entry in catalog.Entries)
        {
            if (entry.Severity >= floor)
            {
                total++;
            }
        }

        return total;
    }

    /// <summary>Projects every entry through <paramref name="projection"/>.</summary>
    /// <typeparam name="TSource">the entry type</typeparam>
    /// <typeparam name="TResult">the projected type</typeparam>
    /// <param name="catalog">the catalogue to read</param>
    /// <param name="projection">applied to each entry</param>
    /// <returns>the projected values, lazily</returns>
    public static IEnumerable<TResult> Select<TSource, TResult>(
        this IReadOnlyCatalog<TSource> catalog,
        Projection<TSource, TResult> projection)
        where TSource : class
    {
        foreach (var entry in catalog.Entries)
        {
            yield return projection(entry);
        }
    }

    /// <summary>Reinterprets a blittable value's bytes.</summary>
    /// <typeparam name="T">the unmanaged value type to read</typeparam>
    /// <param name="source">a pointer to at least <c>sizeof(T)</c> bytes</param>
    /// <returns>the value read from <paramref name="source"/></returns>
    /// <remarks>Exercises an unmanaged constraint and a raw pointer parameter.</remarks>
    public static unsafe T ReadUnaligned<T>(void* source)
        where T : unmanaged => *(T*)source;
}
