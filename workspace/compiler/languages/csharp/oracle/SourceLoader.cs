using System.Collections.Immutable;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;

namespace Nudox.Oracle;

/// <summary>A bound compilation plus the facts the document header needs.</summary>
internal sealed record LoadedCompilation
{
    public required CSharpCompilation Compilation { get; init; }

    public required string AssemblyName { get; init; }

    /// <summary>The target framework moniker the sources were bound against.</summary>
    public required string TargetFramework { get; init; }

    public required int SourceFileCount { get; init; }

    public required int ReferenceCount { get; init; }

    /// <summary>Total compilation errors — the fidelity signal for tiering.</summary>
    public required int ErrorCount { get; init; }

    /// <summary>A capped, human-readable sample of errors for stderr.</summary>
    public required IReadOnlyList<string> ReportableDiagnostics { get; init; }
}

/// <summary>
/// Turns <c>--root</c> directories into a bound <see cref="CSharpCompilation"/>.
/// </summary>
/// <remarks>
/// This is the "source" tier: no MSBuild, no NuGet restore, no project system.
/// Sources are parsed and bound against a fixed set of reference assemblies.
/// Types a package pulls from its own NuGet dependencies therefore bind to
/// <see cref="IErrorTypeSymbol"/>; that is not silently swallowed — every such
/// symbol is counted into the document's <c>diagnostics.errorTypeCount</c> so a
/// consumer can tell a fully-resolved extraction from a partial one.
/// </remarks>
internal static class SourceLoader
{
    /// <summary>Directory names never searched for sources.</summary>
    /// <remarks>
    /// Build output only. Deliberately *not* a heuristic list like "tests" or
    /// "samples": guessing which directories are the library is how an
    /// extraction silently loses half a package. Scope the extraction with
    /// <c>--root</c> instead.
    /// </remarks>
    private static readonly ImmutableHashSet<string> ExcludedDirectories =
        ImmutableHashSet.Create(
            StringComparer.OrdinalIgnoreCase,
            "bin", "obj", ".git", ".vs", "node_modules");

    /// <summary>Errors printed to stderr before the list is truncated.</summary>
    private const int MaxReportedDiagnostics = 25;

    public static LoadedCompilation Load(OracleOptions options)
    {
        var files = CollectSourceFiles(options.Roots);
        if (files.Count == 0)
        {
            throw new OracleFailure(
                $"no .cs files found under: {string.Join(", ", options.Roots)}");
        }

        var assemblyName = options.AssemblyName ?? InferAssemblyName(options.Roots);
        var targetFramework = TargetFrameworkMoniker();

        var parseOptions = new CSharpParseOptions(
            LanguageVersion.Preview,
            // Without this the binder discards doc comments outright and
            // GetDocumentationCommentXml returns empty for every symbol —
            // which would look exactly like "this package has no docs".
            DocumentationMode.Parse,
            SourceCodeKind.Regular,
            PreprocessorSymbols(options.DefineSymbols));

        var sourceTrees = new List<SyntaxTree>(files.Count);
        foreach (var file in files)
        {
            var text = File.ReadAllText(file);
            sourceTrees.Add(CSharpSyntaxTree.ParseText(text, parseOptions, path: file));
        }

        var references = ReferenceAssemblies(options.ReferenceDirectory);
        if (references.Count == 0)
        {
            throw new OracleFailure(
                "no reference assemblies found; pass --ref-dir");
        }

        var compilationOptions = new CSharpCompilationOptions(
            OutputKind.DynamicallyLinkedLibrary,
            allowUnsafe: true,
            // The package's own `#nullable` directives win over this; `Enable`
            // only sets the default for files that say nothing, which is the
            // right default for a modern package and makes `notAnnotated`
            // meaningful rather than universally oblivious.
            nullableContextOptions: NullableContextOptions.Enable,
            // Binding errors are the expected steady state in source mode (a
            // package's NuGet dependencies are not restored). Reporting them as
            // suppressed keeps the diagnostic bag small and fast.
            generalDiagnosticOption: ReportDiagnostic.Suppress);

        var globalUsings = GlobalUsings(options);

        var (compilation, accepted) = BuildWithGlobalUsings(
            assemblyName, sourceTrees, references, compilationOptions, parseOptions, globalUsings);

        if (accepted.Count > 0)
        {
            Console.Error.WriteLine(
                $"oracle: {accepted.Count} global using(s): {string.Join(", ", accepted)}");
        }

        var errors = compilation
            .GetDiagnostics()
            .Where(d => d.Severity == DiagnosticSeverity.Error)
            .ToList();

        var reported = errors
            .Take(MaxReportedDiagnostics)
            .Select(d => d.ToString())
            .ToList();
        if (errors.Count > MaxReportedDiagnostics)
        {
            reported.Add($"... and {errors.Count - MaxReportedDiagnostics} more error(s)");
        }

        return new LoadedCompilation
        {
            Compilation = compilation,
            AssemblyName = assemblyName,
            TargetFramework = targetFramework,
            SourceFileCount = files.Count,
            ReferenceCount = references.Count,
            ErrorCount = errors.Count,
            ReportableDiagnostics = reported,
        };
    }

    /// <summary>All <c>*.cs</c> under the roots, de-duplicated and path-sorted.</summary>
    /// <remarks>
    /// Sorting is what makes the document byte-reproducible: directory
    /// enumeration order is filesystem-defined, and an unstable order would make
    /// every snapshot comparison a coin flip.
    /// </remarks>
    private static List<string> CollectSourceFiles(IReadOnlyList<string> roots)
    {
        var seen = new HashSet<string>(StringComparer.Ordinal);

        foreach (var root in roots)
        {
            if (!Directory.Exists(root))
            {
                throw new OracleFailure($"--root is not a directory: {root}");
            }

            var full = Path.GetFullPath(root);
            foreach (var file in Directory.EnumerateFiles(full, "*.cs", SearchOption.AllDirectories))
            {
                if (IsExcluded(full, file))
                {
                    continue;
                }

                seen.Add(Path.GetFullPath(file));
            }
        }

        var files = seen.ToList();
        files.Sort(StringComparer.Ordinal);
        return files;
    }

    private static bool IsExcluded(string root, string file)
    {
        var relative = Path.GetRelativePath(root, file);
        var segments = relative.Split(
            [Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar],
            StringSplitOptions.RemoveEmptyEntries);

        // The final segment is the file name; only directories are excluded.
        for (var i = 0; i < segments.Length - 1; i++)
        {
            if (ExcludedDirectories.Contains(segments[i]))
            {
                return true;
            }
        }

        return false;
    }

    /// <summary>Path stamped on the reconstructed global-usings tree.</summary>
    private const string GlobalUsingsPath = "<implicit-global-usings>.cs";

    /// <summary>
    /// Build the compilation, discarding any reconstructed global using that
    /// does not bind.
    /// </summary>
    /// <remarks>
    /// The global-usings file is this tool's own reconstruction, not the
    /// package's source, so an error inside it is this tool's bug and must not
    /// be charged to the package's diagnostics — still less be allowed to make a
    /// namespace ambiguous for every file. A using is dropped only when the
    /// compiler rejects it, which happens when a project conditions its
    /// <c>&lt;Using&gt;</c> items on an MSBuild property this tool does not
    /// evaluate. The retry binds only the synthetic tree, so the cost is
    /// negligible and is paid only when something was actually wrong.
    /// </remarks>
    private static (CSharpCompilation Compilation, IReadOnlyList<string> Accepted)
        BuildWithGlobalUsings(
            string assemblyName,
            List<SyntaxTree> sourceTrees,
            List<MetadataReference> references,
            CSharpCompilationOptions compilationOptions,
            CSharpParseOptions parseOptions,
            List<string> requested)
    {
        CSharpCompilation Create(IReadOnlyList<string> usings)
        {
            var trees = new List<SyntaxTree>(sourceTrees.Count + 1);
            trees.AddRange(sourceTrees);

            if (usings.Count > 0)
            {
                // One using per line: a diagnostic's line number is then the
                // index of the namespace that caused it.
                var source = string.Join(
                    '\n', usings.Select(ns => $"global using global::{ns};"));
                trees.Add(CSharpSyntaxTree.ParseText(source, parseOptions, path: GlobalUsingsPath));
            }

            return CSharpCompilation.Create(assemblyName, trees, references, compilationOptions);
        }

        var compilation = Create(requested);
        if (requested.Count == 0)
        {
            return (compilation, requested);
        }

        var syntheticTree = compilation.SyntaxTrees.Single(t => t.FilePath == GlobalUsingsPath);

        var rejected = compilation
            .GetSemanticModel(syntheticTree)
            .GetDiagnostics()
            .Where(d => d.Severity == DiagnosticSeverity.Error)
            .Select(d => d.Location.GetLineSpan().StartLinePosition.Line)
            .Where(line => line >= 0 && line < requested.Count)
            .Select(line => requested[line])
            .ToHashSet(StringComparer.Ordinal);

        if (rejected.Count == 0)
        {
            return (compilation, requested);
        }

        var accepted = requested.Where(ns => !rejected.Contains(ns)).ToList();
        Console.Error.WriteLine(
            $"oracle: dropped {rejected.Count} unbindable global using(s): "
                + string.Join(", ", rejected.Order(StringComparer.Ordinal)));

        return (Create(accepted), accepted);
    }

    /// <summary>
    /// Namespaces the SDK makes globally visible for a <c>Microsoft.NET.Sdk</c>
    /// library when <c>ImplicitUsings</c> is enabled.
    /// </summary>
    private static readonly string[] SdkImplicitUsings =
    [
        "System",
        "System.Collections.Generic",
        "System.IO",
        "System.Linq",
        "System.Net.Http",
        "System.Threading",
        "System.Threading.Tasks",
    ];

    /// <summary>
    /// The global usings to compile alongside the sources.
    /// </summary>
    /// <remarks>
    /// MSBuild generates a <c>*.GlobalUsings.g.cs</c> file into <c>obj/</c> from
    /// the <c>ImplicitUsings</c> property and any <c>&lt;Using&gt;</c> items.
    /// That directory is build output and is not searched, so the file has to be
    /// reconstructed here — otherwise a package that relies on it binds
    /// <c>System</c> types to nothing and the extraction quietly degrades into
    /// error types rather than failing.
    /// </remarks>
    private static List<string> GlobalUsings(OracleOptions options)
    {
        var usings = new List<string>();
        var seen = new HashSet<string>(StringComparer.Ordinal);

        void Add(string ns)
        {
            if (!string.IsNullOrWhiteSpace(ns) && seen.Add(ns))
            {
                usings.Add(ns);
            }
        }

        if (options.ImplicitUsings)
        {
            foreach (var ns in SdkImplicitUsings)
            {
                Add(ns);
            }
        }

        var (included, removed) = ProjectUsings(options.Roots);

        foreach (var ns in included)
        {
            Add(ns);
        }

        // `<Using Remove="…"/>` is how a project opts out of one of the SDK's
        // implicit usings; ignoring it would make a name visible that the
        // package deliberately hid.
        usings.RemoveAll(removed.Contains);

        // An explicit `--using` is the operator's instruction and outranks a
        // project-level removal.
        foreach (var ns in options.ExtraUsings)
        {
            Add(ns);
        }

        return usings;
    }

    /// <summary>How far above a root to look for auto-imported MSBuild files.</summary>
    private const int MaxAncestorDepth = 8;

    /// <summary>
    /// <c>&lt;Using&gt;</c> items declared by the project file and by the
    /// <c>Directory.Build.props</c>/<c>.targets</c> files MSBuild imports
    /// automatically from ancestor directories.
    /// </summary>
    /// <remarks>
    /// <para>
    /// Read as plain XML rather than by evaluating MSBuild: the goal is to
    /// recover the handful of namespaces a package adds to every file, not to
    /// reimplement the project system. Only the two file names MSBuild imports
    /// by convention are read — chasing arbitrary <c>&lt;Import&gt;</c> elements
    /// would be reimplementing evaluation badly.
    /// </para>
    /// <para>
    /// An item inside a conditioned <c>&lt;ItemGroup&gt;</c> is skipped. The
    /// condition depends on properties this tool does not evaluate, so its value
    /// is unknown — and adding a namespace that should have been excluded is
    /// worse than omitting it, because a <c>global using</c> of a namespace that
    /// does not exist is a hard error in every file.
    /// </para>
    /// <para>
    /// Items carrying <c>Static</c> or <c>Alias</c> are skipped because those
    /// forms change name binding in ways a partial emulation would get wrong.
    /// </para>
    /// </remarks>
    private static (IReadOnlyList<string> Included, IReadOnlySet<string> Removed) ProjectUsings(
        IReadOnlyList<string> roots)
    {
        var included = new List<string>();
        var removed = new HashSet<string>(StringComparer.Ordinal);
        var visited = new HashSet<string>(StringComparer.Ordinal);

        foreach (var candidate in CandidateProjectFiles(roots))
        {
            if (!visited.Add(candidate))
            {
                continue;
            }

            System.Xml.Linq.XDocument document;
            try
            {
                document = System.Xml.Linq.XDocument.Load(candidate);
            }
            catch (Exception ex) when (ex is System.Xml.XmlException or IOException)
            {
                continue;
            }

            foreach (var item in document.Descendants("Using"))
            {
                if (item.Attribute("Static") is not null || item.Attribute("Alias") is not null)
                {
                    continue;
                }

                if (item.Attribute("Condition") is not null
                    || item.Parent?.Attribute("Condition") is not null)
                {
                    continue;
                }

                if (item.Attribute("Remove")?.Value is { Length: > 0 } remove)
                {
                    removed.Add(remove);
                }

                if (item.Attribute("Include")?.Value is { Length: > 0 } include)
                {
                    included.Add(include);
                }
            }
        }

        return (included, removed);
    }

    /// <summary>Project files whose <c>&lt;Using&gt;</c> items apply to the roots.</summary>
    private static IEnumerable<string> CandidateProjectFiles(IReadOnlyList<string> roots)
    {
        foreach (var root in roots)
        {
            if (!Directory.Exists(root))
            {
                continue;
            }

            var directory = new DirectoryInfo(Path.GetFullPath(root));

            foreach (var project in Directory
                         .EnumerateFiles(directory.FullName, "*.csproj", SearchOption.TopDirectoryOnly)
                         .OrderBy(p => p, StringComparer.Ordinal))
            {
                yield return project;
            }

            for (var depth = 0; directory is not null && depth < MaxAncestorDepth; depth++)
            {
                foreach (var name in new[] { "Directory.Build.props", "Directory.Build.targets" })
                {
                    var path = Path.Combine(directory.FullName, name);
                    if (File.Exists(path))
                    {
                        yield return path;
                    }
                }

                directory = directory.Parent;
            }
        }
    }

    /// <summary>
    /// Guess the assembly name from the roots: a sibling project file's name,
    /// else the deepest root directory's name.
    /// </summary>
    /// <remarks>
    /// This matters more than it looks: <c>PackageSource::name</c> is a lookup
    /// key downstream, and a wrong name reads as an empty extraction rather than
    /// as a naming mistake.
    /// </remarks>
    private static string InferAssemblyName(IReadOnlyList<string> roots)
    {
        foreach (var root in roots)
        {
            if (!Directory.Exists(root))
            {
                continue;
            }

            var projects = Directory
                .EnumerateFiles(root, "*.csproj", SearchOption.TopDirectoryOnly)
                .OrderBy(p => p, StringComparer.Ordinal)
                .ToList();

            if (projects.Count > 0)
            {
                return Path.GetFileNameWithoutExtension(projects[0]);
            }
        }

        var first = Path.GetFullPath(roots[0]).TrimEnd(
            Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar);
        var name = Path.GetFileName(first);
        return string.IsNullOrEmpty(name) ? "assembly" : name;
    }

    /// <summary>
    /// The `#define` set the .NET SDK would have supplied for this TFM.
    /// </summary>
    /// <remarks>
    /// Omitting these is not cosmetic: a package that wraps modern API in
    /// <c>#if NET8_0_OR_GREATER</c> would have that code parsed as disabled text
    /// and every symbol inside it would silently vanish from the extraction.
    /// </remarks>
    private static IEnumerable<string> PreprocessorSymbols(IReadOnlyList<string> extra)
    {
        var major = Environment.Version.Major;

        var symbols = new List<string> { "NET", $"NET{major}_0", "NETCOREAPP", "RELEASE", "TRACE" };

        // `NETn_0_OR_GREATER` exists from .NET 5; `NETCOREAPPn_0_OR_GREATER`
        // covers the .NET Core lineage a package may still be testing for.
        for (var v = 5; v <= major; v++)
        {
            symbols.Add($"NET{v}_0_OR_GREATER");
        }

        foreach (var v in new[] { "1_0", "1_1", "2_0", "2_1", "2_2", "3_0", "3_1" })
        {
            symbols.Add($"NETCOREAPP{v}_OR_GREATER");
        }

        symbols.AddRange(extra);
        return symbols;
    }

    private static string TargetFrameworkMoniker() => $"net{Environment.Version.Major}.0";

    /// <summary>
    /// Reference assemblies for binding.
    /// </summary>
    /// <remarks>
    /// Defaults to the directory of the runtime this oracle is itself executing
    /// on, which is guaranteed to exist and to be self-consistent. An SDK
    /// reference pack is a fine alternative via <c>--ref-dir</c>, but making the
    /// default depend on SDK layout would turn a missing pack into a confusing
    /// "everything is an error type" extraction.
    /// </remarks>
    private static List<MetadataReference> ReferenceAssemblies(string? overrideDirectory)
    {
        var directory = overrideDirectory
            ?? Path.GetDirectoryName(typeof(object).Assembly.Location);

        if (string.IsNullOrEmpty(directory) || !Directory.Exists(directory))
        {
            return [];
        }

        var references = new List<MetadataReference>();
        var files = Directory.GetFiles(directory, "*.dll");
        Array.Sort(files, StringComparer.Ordinal);

        foreach (var file in files)
        {
            var name = Path.GetFileNameWithoutExtension(file);

            // The native host shims are not managed assemblies; handing them to
            // Roslyn throws rather than returning an error.
            if (name.StartsWith("api-ms-", StringComparison.OrdinalIgnoreCase)
                || name.StartsWith("clr", StringComparison.OrdinalIgnoreCase)
                || name.Equals("hostfxr", StringComparison.OrdinalIgnoreCase)
                || name.Equals("hostpolicy", StringComparison.OrdinalIgnoreCase)
                || name.Equals("Microsoft.DiaSymReader.Native.amd64", StringComparison.OrdinalIgnoreCase))
            {
                continue;
            }

            try
            {
                references.Add(MetadataReference.CreateFromFile(file));
            }
            catch (Exception ex) when (ex is BadImageFormatException or IOException)
            {
                // A native or corrupt file in the runtime directory is not a
                // reason to fail the whole extraction; note it and move on.
                Console.Error.WriteLine($"oracle: skipping reference {name}: {ex.Message}");
            }
        }

        return references;
    }
}
