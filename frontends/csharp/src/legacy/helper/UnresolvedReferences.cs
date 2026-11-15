using System.Globalization;
using System.Text;
using System.Text.Json;
using System.Text.RegularExpressions;
using Microsoft.CodeAnalysis;

namespace Nudox.Oracle;

/// <summary>
/// Names the first reference Roslyn could not resolve, in a fixed order, so a refused
/// project says which package, project or namespace is missing instead of only that
/// "the inputs" were refused.
/// </summary>
internal static class UnresolvedReferences
{
    /// <summary>
    /// Compiler errors that mean a type, namespace or assembly is not available to the
    /// compilation (as opposed to an ordinary error in the project's own code).
    /// </summary>
    private static readonly HashSet<string> Codes = new(StringComparer.Ordinal)
    {
        "CS0246", "CS0234", "CS0012", "CS0400", "CS0006", "CS0518",
    };

    private const int MaxSymbolBytes = 96;

    /// <summary>
    /// A one-line JSON summary of the earliest unresolved reference (ordered by
    /// package-relative path, line, column and code), or null when every error is of
    /// another kind. The sample of reported diagnostics is capped and unordered; this
    /// summary is computed over every error.
    /// </summary>
    public static string? Summarize(IEnumerable<Diagnostic> errors, string root, RestoredPackages packages)
    {
        var found = new List<(string Path, int Line, int Column, string Code, string Symbol)>();
        foreach (var error in errors)
        {
            if (!Codes.Contains(error.Id) || !error.Location.IsInSource) continue;
            var path = error.Location.SourceTree?.FilePath;
            if (string.IsNullOrEmpty(path) || !System.IO.Path.IsPathRooted(path)) continue;
            var relative = System.IO.Path.GetRelativePath(root, path).Replace('\\', '/');
            if (relative.StartsWith("../", StringComparison.Ordinal) || relative == "..") continue;
            var position = error.Location.GetLineSpan().StartLinePosition;
            found.Add((relative, position.Line + 1, position.Character + 1, error.Id,
                Symbol(error.Id, error.GetMessage(CultureInfo.InvariantCulture))));
        }

        if (found.Count == 0) return null;
        found.Sort((left, right) =>
        {
            var order = string.CompareOrdinal(left.Path, right.Path);
            if (order != 0) return order;
            order = left.Line.CompareTo(right.Line);
            if (order != 0) return order;
            order = left.Column.CompareTo(right.Column);
            return order != 0 ? order : string.CompareOrdinal(left.Code, right.Code);
        });
        var first = found[0];
        var project = RestoredPackages.OwningProject(root, first.Path);
        var restore = project is null || packages.ProjectCount == 0
            ? "unknown"
            : packages.UnrestoredProjects.Contains(project) ? "missing" : "present";
        using var stream = new MemoryStream();
        using (var json = new Utf8JsonWriter(stream))
        {
            json.WriteStartObject();
            json.WriteString("code", first.Code);
            json.WriteString("symbol", first.Symbol);
            json.WriteString("path", first.Path);
            json.WriteNumber("line", first.Line);
            if (project is not null) json.WriteString("project", project);
            json.WriteString("restore", restore);
            json.WriteNumber("count", found.Count);
            json.WriteNumber("files", found.Select(item => item.Path).Distinct(StringComparer.Ordinal).Count());
            json.WriteEndObject();
        }

        return Encoding.UTF8.GetString(stream.ToArray());
    }

    private static string Symbol(string code, string message)
    {
        var quoted = Regex.Matches(message, "'([^']+)'").Select(match => match.Groups[1].Value).ToList();
        if (quoted.Count == 0) return "?";
        var text = code switch
        {
            // "The type or namespace name 'X' does not exist in the namespace 'N'".
            "CS0234" when quoted.Count > 1 => quoted[1] + "." + quoted[0],
            // "Metadata file '<path>' could not be found": the file name only.
            "CS0006" => System.IO.Path.GetFileName(quoted[0].Replace('\\', '/')),
            _ => quoted[0],
        };
        var builder = new StringBuilder(MaxSymbolBytes);
        foreach (var character in text)
        {
            if (builder.Length >= MaxSymbolBytes) break;
            builder.Append(char.IsAsciiLetterOrDigit(character) || character is '_' or '.' or '<' or '>' or ',' or '`' or '+'
                ? character
                : '_');
        }

        return builder.Length == 0 ? "?" : builder.ToString();
    }
}
