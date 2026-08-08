using System.ComponentModel;

namespace Nudox.Fixture;

/// <summary>
/// Marks a type as tracked by the catalogue.
/// </summary>
/// <remarks>
/// Exists so the extraction has a <i>user-defined</i> attribute to carry, not
/// only framework ones.
/// </remarks>
[AttributeUsage(AttributeTargets.Class | AttributeTargets.Struct, AllowMultiple = false)]
public sealed class TrackedAttribute : Attribute
{
    /// <summary>Creates the marker with a channel name.</summary>
    /// <param name="channel">the channel the type reports on</param>
    public TrackedAttribute(string channel) => Channel = channel;

    /// <summary>The channel the type reports on.</summary>
    public string Channel { get; }

    /// <summary>Whether the type is reported at startup.</summary>
    public bool Eager { get; set; }
}

/// <summary>How serious an entry is.</summary>
public enum Severity
{
    /// <summary>Informational only.</summary>
    Info = 0,

    /// <summary>Something to look at.</summary>
    Warning = 10,

    /// <summary>Something broke.</summary>
    Error = 20,
}

/// <summary>Behaviours that can be combined.</summary>
[Flags]
public enum CatalogOptions : byte
{
    /// <summary>No behaviour.</summary>
    None = 0,

    /// <summary>Keep entries after a read.</summary>
    Retain = 1,

    /// <summary>Compact on write.</summary>
    Compact = 2,
}

/// <summary>Projects a <typeparamref name="TSource"/> into a <typeparamref name="TResult"/>.</summary>
/// <typeparam name="TSource">the input type</typeparam>
/// <typeparam name="TResult">the output type</typeparam>
/// <param name="source">the value to project</param>
/// <returns>the projected value</returns>
public delegate TResult Projection<in TSource, out TResult>(TSource source);

/// <summary>Anything the catalogue can store.</summary>
public interface IEntry
{
    /// <summary>The stable key.</summary>
    string Key { get; }

    /// <summary>How serious this entry is.</summary>
    Severity Severity { get; }
}

/// <summary>A read-only view whose element type is covariant.</summary>
/// <typeparam name="T">the element type</typeparam>
public interface IReadOnlyCatalog<out T>
    where T : class
{
    /// <summary>The number of entries.</summary>
    int Count { get; }

    /// <summary>Every entry, in insertion order.</summary>
    IEnumerable<T> Entries { get; }
}

/// <summary>A point on the page.</summary>
/// <param name="X">the horizontal offset</param>
/// <param name="Y">the vertical offset</param>
/// <seealso cref="Extent"/>
public record Point(double X, double Y)
{
    /// <summary>The top-left corner.</summary>
    public static readonly Point Origin = new(0, 0);

    /// <summary>An optional label. <see langword="null"/> when unlabelled.</summary>
    public string? Label { get; init; }
}

/// <summary>A width and height.</summary>
/// <param name="Width">the width in cells</param>
/// <param name="Height">the height in cells</param>
public readonly record struct Extent(int Width, int Height)
{
    /// <summary>The area covered.</summary>
    public int Area => Width * Height;
}

/// <summary>An entry that failed to load.</summary>
public sealed class BrokenEntryException : Exception
{
    /// <summary>Creates the exception for a key.</summary>
    /// <param name="key">the key that failed</param>
    public BrokenEntryException(string key)
        : base($"entry '{key}' is broken") => Key = key;

    /// <summary>The key that failed.</summary>
    public string Key { get; }
}
