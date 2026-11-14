using System.Collections.Immutable;
using System.Globalization;
using System.Reflection;
using System.Reflection.Metadata;
using System.Reflection.PortableExecutable;
using System.Security.Cryptography;
using System.Text.RegularExpressions;
using System.Text;
using System.Text.Json;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;
using Microsoft.CodeAnalysis.Diagnostics;

namespace Nudox.Oracle;

/// <summary>A bound compilation plus the facts the document header needs.</summary>
internal sealed record CapturedReferenceWitness(string Name, long Length, string Sha256);

internal sealed record LoadedCompilation
{
    /// <summary>Captured bytes, when supplied by the compiler owner.</summary>
    public IReadOnlyDictionary<string, byte[]>? CapturedBytes { get; init; }
    public string? CapturedConfigDigest { get; init; }
    public IReadOnlyList<CapturedReferenceWitness>? CapturedReferences { get; init; }
    public IReadOnlySet<SyntaxTree>? CapturedGraphTrees { get; init; }
    public IReadOnlyList<string>? CapturedAcceptedUsings { get; init; }
    public IReadOnlyList<string>? CapturedDefines { get; init; }
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

    /// <summary>Whether configured source generators ran successfully.</summary>
    public required string GeneratorSupport { get; init; }

    /// <summary>
    /// The absolute, fully-resolved effective root every bound
    /// <see cref="Microsoft.CodeAnalysis.SyntaxTree.FilePath"/> was found
    /// under (the first <c>--root</c>, narrowed by
    /// <see cref="SourceLoader.ScopeToMatchingProject"/> exactly as the
    /// bound sources themselves were).
    /// </summary>
    /// <remarks>
    /// Every other declaration-identity path in this engine — see
    /// <c>DeclarationScope</c>'s "package-relative path" — is relative to a
    /// package root, never a raw OS absolute path. <see cref="AuthorityImage"/>
    /// uses this to embed the same convention instead of the literal
    /// filesystem location the oracle happened to run from, which is
    /// machine- and checkout-specific and therefore not reproducible.
    /// </remarks>
    public required string Root { get; init; }
}

/// <summary>
/// Turns <c>--root</c> directories into a bound <see cref="CSharpCompilation"/>.
/// </summary>
/// <remarks>
/// This is the "source" tier: it does not evaluate MSBuild or restore NuGet
/// packages. Sources are parsed and bound against a fixed set of reference
/// assemblies; explicitly listed project analyzers are loaded only to execute
/// their source generators.
/// Types a package pulls from its own NuGet dependencies therefore bind to
/// <see cref="IErrorTypeSymbol"/>; that is not silently swallowed — every such
/// symbol is counted into the document's <c>diagnostics.errorTypeCount</c> so a
/// consumer can tell a fully-resolved extraction from a partial one.
///
/// One narrow exception to "walks every <c>.cs</c> file unconditionally":
/// <see cref="ApplyTfmPreference"/>. dotnet/runtime-style packages ship
/// per-TFM source *file* variants selected by MSBuild <c>&lt;Compile
/// Include&gt;</c> conditions — e.g. <c>Foo.netstandard.cs</c> and
/// <c>Foo.net8.cs</c> both declaring the same member on the same <c>partial
/// class</c>, with only one ever compiled into a given TFM's assembly. This
/// tool cannot evaluate that MSBuild condition (see the class remarks above),
/// so without this step every such pair binds as a hard duplicate-definition
/// error instead of a clean extraction. The heuristic is deliberately narrow:
/// it only ever picks among files that already share a directory and a base
/// name after stripping a recognized TFM tag, and a lone TFM-tagged file with
/// no such sibling is always kept untouched.
/// </remarks>
internal static partial class SourceLoader
{
    private sealed record CapturedSource(string Path, string Text);
    private sealed record CapturedConfig(string Path, string? Text);

    // The owner supplies the exact ordered project and in-package ancestor probes.
    // Null Text is a captured absence, not permission to reread the path.
    private static (IReadOnlyList<string> Documents, string Digest) ReadCapturedConfig(string root, string projectFile, string manifestPath)
        => ParseCapturedConfig(root, projectFile, ReadBoundedManifest(manifestPath, 16 * 1024 * 1024));

    private static string ReadBoundedManifest(string manifestPath, int maximum)
    {
        string json;
        try
        {
            using var reader = new StreamReader(manifestPath, new UTF8Encoding(false, true), detectEncodingFromByteOrderMarks: false);
            var buffer = new char[8192];
            var text = new StringBuilder();
            int count;
            while ((count = reader.Read(buffer, 0, buffer.Length)) != 0)
            {
                if (text.Length > maximum - count)
                    throw new OracleFailure("captured config manifest exceeds limit");
                text.Append(buffer, 0, count);
            }
            json = text.ToString();
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or DecoderFallbackException)
        {
            throw new OracleFailure("captured config manifest could not be read");
        }
        return json;
    }

    private static (IReadOnlyList<string> Documents, string Digest) ParseCapturedConfig(string root, string projectFile, string json)
    {
        CapturedConfig[] entries;
        try { entries = JsonSerializer.Deserialize<CapturedConfig[]>(json) ?? throw new OracleFailure("empty captured configuration"); }
        catch (JsonException) { throw new OracleFailure("invalid captured configuration"); }
        var fullRoot = Path.GetFullPath(root).TrimEnd(Path.DirectorySeparatorChar);
        var fullProject = Path.GetFullPath(projectFile);
        var relativeProject = Path.GetRelativePath(fullRoot, fullProject).Replace('\\', '/');
        if (Path.IsPathRooted(relativeProject) || relativeProject.Split('/').Any(part => part is "" or "." or "..")
            || !relativeProject.EndsWith(".csproj", StringComparison.OrdinalIgnoreCase))
            throw new OracleFailure("captured project is outside package root");
        var expected = new List<string> { relativeProject };
        var current = Path.GetDirectoryName(fullProject);
        var reachedRoot = false;
        for (var depth = 0; current is not null && depth < MaxAncestorDepth; depth++)
        {
            var relative = Path.GetRelativePath(fullRoot, current).Replace('\\', '/');
            if (relative == ".." || relative.StartsWith("../", StringComparison.Ordinal)) break;
            var prefix = relative == "." ? "" : relative + "/";
            expected.Add(prefix + "Directory.Build.props");
            expected.Add(prefix + "Directory.Build.targets");
            if (relative == ".") { reachedRoot = true; break; }
            current = Path.GetDirectoryName(current);
        }
        if (!reachedRoot) throw new OracleFailure("captured config project exceeds scope depth");
        if (entries.Length != expected.Count || entries.Length > 17)
            throw new OracleFailure("captured config probes do not match project scope");
        var output = new List<string>();
        for (var i = 0; i < entries.Length; i++)
        {
            if (entries[i] is null || entries[i].Path != expected[i] || (i == 0 && entries[i].Text is null))
                throw new OracleFailure("captured config probes do not match project scope");
            if (entries[i].Text is { } text) output.Add(text);
        }
        return (output, Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes(json))).ToLowerInvariant());
    }

    private static Dictionary<string, byte[]> ReadCapturedSources(string root, IReadOnlyList<string> roots, TextReader reader)
    {
        const int maxChars = 64 * 1024 * 1024;
        var input = new StringBuilder();
        var buffer = new char[8192];
        int count;
        while ((count = reader.Read(buffer, 0, buffer.Length)) != 0)
        {
            if (input.Length > maxChars - count) throw new OracleFailure("captured source manifest exceeds limit");
            input.Append(buffer, 0, count);
        }
        CapturedSource[] entries;
        try
        {
            entries = JsonSerializer.Deserialize<CapturedSource[]>(input.ToString())
                ?? throw new OracleFailure("captured source manifest is empty");
        }
        catch (JsonException ex) { throw new OracleFailure($"invalid captured source manifest: {ex.Message}"); }
        if (entries.Length > 8192) throw new OracleFailure("captured source set exceeds file limit");
        var result = new Dictionary<string, byte[]>(StringComparer.Ordinal);
        var fullRoot = Path.GetFullPath(root);
        foreach (var entry in entries)
        {
            if (entry is null || string.IsNullOrEmpty(entry.Path) || Encoding.UTF8.GetByteCount(entry.Path) > 4096
                || entry.Text is null || Path.IsPathRooted(entry.Path)
                || entry.Path.Split('/').Any(segment => segment is "" or "." or "..")
                || entry.Path.Contains('\\'))
                throw new OracleFailure("captured source has invalid relative path");
            var full = Path.GetFullPath(Path.Combine(fullRoot, entry.Path));
            if (!roots.Any(r => full.StartsWith(Path.GetFullPath(r).TrimEnd(Path.DirectorySeparatorChar) + Path.DirectorySeparatorChar, StringComparison.Ordinal))
                || !entry.Path.EndsWith(".cs", StringComparison.OrdinalIgnoreCase)
                || !result.TryAdd(full, Encoding.UTF8.GetBytes(entry.Text)))
                throw new OracleFailure("captured source is outside the selected project or duplicated");
        }
        return result;
    }
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
        => options.CapturedProjectGraphPath is { } graphPath ? LoadCapturedGraph(options, graphPath) : LoadCore(options);

    private static LoadedCompilation LoadCore(OracleOptions options,
        Dictionary<string, byte[]>? suppliedSources = null,
        (IReadOnlyList<string> Documents, string Digest)? suppliedConfig = null,
        IReadOnlyList<MetadataReference>? projectReferences = null)
    {
        // Narrow to the one project the caller actually asked for, when the
        // evidence to do so unambiguously exists. See `ScopeToMatchingProject`
        // for why this exists and what it deliberately does not do.
        var identityRoot = Path.GetFullPath(options.Roots[0]);
        (IReadOnlyList<string> Documents, string Digest)? config = suppliedConfig ?? (options.CapturedConfigPath is { } configPath && options.ProjectFile is { } configProject
            ? ReadCapturedConfig(identityRoot, configProject, configPath)
            : null);
        var roots = options.ProjectFile is { } exactProject
            ? ScopeToExactProject(options.Roots, exactProject, config is null)
            : ScopeToMatchingProject(options.Roots, options.AssemblyName);

        Dictionary<string, byte[]>? captured = suppliedSources;
        if (suppliedSources is null && options.CapturedSourcesPath is { } manifestPath)
        {
            try
            {
                using var reader = File.OpenText(manifestPath);
                captured = ReadCapturedSources(identityRoot, roots, reader);
            }
            catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
            {
                throw new OracleFailure("captured source manifest could not be read");
            }
        }
        else if (suppliedSources is null && options.CapturedSources)
        {
            captured = ReadCapturedSources(identityRoot, roots, Console.In);
        }
        var files = captured is null ? CollectSourceFiles(roots) : captured.Keys.Order(StringComparer.Ordinal).ToList();
        if (files.Count == 0)
        {
            throw new OracleFailure(
                $"no .cs files found under: {string.Join(", ", roots)}");
        }

        var assemblyName = options.AssemblyName ?? (config is not null && options.ProjectFile is { } selectedProject
            ? Path.GetFileNameWithoutExtension(selectedProject) : InferAssemblyName(roots));
        var targetFramework = options.TargetFramework ?? TargetFrameworkMoniker();

        var parseOptions = new CSharpParseOptions(
            options.LanguageVersion,
            // Without this the binder discards doc comments outright and
            // GetDocumentationCommentXml returns empty for every symbol —
            // which would look exactly like "this package has no docs".
            DocumentationMode.Parse,
            SourceCodeKind.Regular,
            PreprocessorSymbols(options.DefineSymbols, options.ExactDefineSymbols));

        var sourceTrees = new List<SyntaxTree>(files.Count);
        foreach (var file in files)
        {
            var text = captured is null ? File.ReadAllText(file) : Encoding.UTF8.GetString(captured[file]);
            if (captured is not null && text.StartsWith('\uFEFF')) text = text[1..];
            sourceTrees.Add(CSharpSyntaxTree.ParseText(text, parseOptions, path: file));
        }

        // Without caller-selected reference directories the package's own project
        // files decide whether the ASP.NET Core shared framework is part of the
        // compilation. A Web SDK project that cannot see Microsoft.AspNetCore.* would
        // otherwise report hundreds of unresolved-type errors for healthy code.
        // A fully captured unit (exact configuration probes) receives its references
        // from the host's SDK observation instead, and never reads project files here.
        // Project files count as compiler configuration for freshness, so a change
        // to one invalidates the published result that this choice shaped.
        var detectedFrameworks = config is null && options.ReferenceDirectories.Count == 0
            ? DetectSharedFrameworks(roots)
            : DetectedFrameworks.None;
        var (references, capturedReferences) = ReferenceAssemblies(
            options.ReferenceDirectories, captured is not null, detectedFrameworks.ReferenceDirectories);
        if (projectReferences is not null) references.AddRange(projectReferences);
        if (detectedFrameworks.ReferenceDirectories.Count > 0)
        {
            Console.Error.WriteLine(
                $"oracle: project files select {string.Join(", ", detectedFrameworks.Names)}; "
                    + $"added reference directories: {string.Join(", ", detectedFrameworks.ReferenceDirectories)}");
        }
        if (references.Count == 0)
        {
            throw new OracleFailure(
                "no reference assemblies found; pass --ref-dir");
        }

        // Roslyn rejects top-level statements in a library (CS8805). Select
        // an executable only when the admitted source actually contains them;
        // ordinary libraries must not acquire a synthetic entry point.
        var hasTopLevelStatements = sourceTrees.Any(tree =>
            tree.GetRoot() is CompilationUnitSyntax unit
            && unit.Members.Any(member => member is GlobalStatementSyntax));
        var compilationOptions = new CSharpCompilationOptions(
            options.ExactOutputType switch {
                "Exe" => OutputKind.ConsoleApplication,
                "Library" => OutputKind.DynamicallyLinkedLibrary,
                "WinExe" => OutputKind.WindowsApplication,
                _ => hasTopLevelStatements ? OutputKind.ConsoleApplication : OutputKind.DynamicallyLinkedLibrary,
            },
            allowUnsafe: options.ExactUnsafe ?? true,
            // The package's own `#nullable` directives win over this; `Enable`
            // only sets the default for files that say nothing, which is the
            // right default for a modern package and makes `notAnnotated`
            // meaningful rather than universally oblivious.
            nullableContextOptions: options.ExactNullable switch {
                "disable" => NullableContextOptions.Disable,
                "warnings" => NullableContextOptions.Warnings,
                "annotations" => NullableContextOptions.Annotations,
                _ => NullableContextOptions.Enable,
            },
            // Binding errors are the expected steady state in source mode (a
            // package's NuGet dependencies are not restored). Reporting them as
            // suppressed keeps the diagnostic bag small and fast.
            generalDiagnosticOption: ReportDiagnostic.Suppress);

        var globalUsings = GlobalUsings(options, roots, config?.Documents, detectedFrameworks);

        var (compilation, accepted) = BuildWithGlobalUsings(
            assemblyName, sourceTrees, references, compilationOptions, parseOptions, globalUsings);

        if (accepted.Count > 0)
        {
            Console.Error.WriteLine(
                $"oracle: {accepted.Count} global using(s): {string.Join(", ", accepted)}");
        }

        // Captured source text alone does not authenticate analyzer binaries,
        // their inputs, or generated output. Never load or execute configured
        // generators in this bounded capture mode; coverage remains unknown.
        var (generatedCompilation, generatorSupport, generatorDiagnostics) = captured is null
            ? ApplyConfiguredGenerators(compilation, parseOptions, roots)
            : (compilation, "unknown", (IReadOnlyList<Diagnostic>)Array.Empty<Diagnostic>());
        compilation = generatedCompilation;

        var errors = compilation
            .GetDiagnostics()
            .Where(d => d.Severity == DiagnosticSeverity.Error)
            .ToList();
        errors.AddRange(
            generatorDiagnostics.Where(d => d.Severity == DiagnosticSeverity.Error));

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
            CapturedBytes = captured,
            CapturedConfigDigest = config?.Digest,
            CapturedReferences = capturedReferences,
            CapturedAcceptedUsings = captured is null ? null : accepted,
            CapturedDefines = captured is null ? null : parseOptions.PreprocessorSymbolNames.ToArray(),
            Compilation = compilation,
            AssemblyName = assemblyName,
            TargetFramework = targetFramework,
            SourceFileCount = files.Count,
            ReferenceCount = references.Count,
            ErrorCount = errors.Count,
            ReportableDiagnostics = reported,
            GeneratorSupport = generatorSupport,
            Root = identityRoot,
        };
    }

    /// <summary>Scopes source loading to one exact admitted project file.</summary>
    private static IReadOnlyList<string> ScopeToExactProject(
        IReadOnlyList<string> packageRoots, string projectFile, bool requirePhysicalProject = true)
    {
        var fullProject = Path.GetFullPath(projectFile);
        var packageRoot = Path.GetFullPath(packageRoots[0]);
        var relative = Path.GetRelativePath(packageRoot, fullProject);
        if (relative == "."
            || Path.IsPathRooted(relative)
            || relative == ".."
            || relative.StartsWith(".." + Path.DirectorySeparatorChar, StringComparison.Ordinal)
            || relative.StartsWith(".." + Path.AltDirectorySeparatorChar, StringComparison.Ordinal)
            || (requirePhysicalProject && !File.Exists(fullProject))
            || !string.Equals(Path.GetExtension(fullProject), ".csproj", StringComparison.OrdinalIgnoreCase))
        {
            throw new OracleFailure("--project-file must name one .csproj beneath the package root");
        }

        var projectDirectory = Path.GetDirectoryName(fullProject);
        if (projectDirectory is null)
        {
            throw new OracleFailure("--project-file has no containing directory");
        }
        return [projectDirectory];
    }

    /// <summary>
    /// Load and execute analyzers declared by the selected project(s).
    /// </summary>
    /// <remarks>
    /// A source generator is part of the project's compilation inputs, not an
    /// optional decoration. Missing analyzer paths therefore produce the
    /// explicit <c>unavailable</c> state rather than an apparently complete
    /// extraction with generated members absent.
    /// </remarks>
    private static (
        CSharpCompilation Compilation,
        string Support,
        IReadOnlyList<Diagnostic> Diagnostics)
        ApplyConfiguredGenerators(
            CSharpCompilation compilation,
            CSharpParseOptions parseOptions,
            IReadOnlyList<string> roots)
    {
        var configured = ProjectAnalyzerPaths(roots).ToList();
        if (configured.Count == 0)
        {
            return (compilation, "unknown", []);
        }

        var missing = configured.Where(path => !File.Exists(path)).ToList();
        if (missing.Count > 0)
        {
            foreach (var path in missing)
            {
                Console.Error.WriteLine($"oracle: source generator unavailable: {path}");
            }

            return (compilation, "unavailable", []);
        }

        try
        {
            var loader = new AnalyzerAssemblyLoader();
            var references = configured
                .Select(path => new AnalyzerFileReference(path, loader))
                .ToImmutableArray();
            var generators = references
                .SelectMany(reference => reference.GetGenerators(LanguageNames.CSharp))
                .ToImmutableArray();

            if (generators.Length == 0)
            {
                return (compilation, "unavailable", []);
            }

            GeneratorDriver driver = CSharpGeneratorDriver.Create(
                generators, parseOptions: parseOptions);
            driver = driver.RunGeneratorsAndUpdateCompilation(
                compilation, out var updated, out var diagnostics);
            _ = driver;
            return ((CSharpCompilation)updated, "applied", diagnostics);
        }
        catch (Exception ex) when (
            ex is IOException
                or BadImageFormatException
                or FileLoadException
                or FileNotFoundException
                or InvalidOperationException
                or ArgumentException
                or NotSupportedException
                or TypeLoadException
                or ReflectionTypeLoadException)
        {
            Console.Error.WriteLine($"oracle: source generator unavailable: {ex.Message}");
            return (compilation, "unavailable", []);
        }
    }

    private sealed class AnalyzerAssemblyLoader : IAnalyzerAssemblyLoader
    {
        public void AddDependencyLocation(string fullPath) { }

        public Assembly LoadFromPath(string path) => Assembly.LoadFrom(path);
    }

    private static IEnumerable<string> ProjectAnalyzerPaths(IReadOnlyList<string> roots)
    {
        foreach (var root in roots)
        {
            if (!Directory.Exists(root))
            {
                continue;
            }

            foreach (var project in Directory.EnumerateFiles(
                         root, "*.csproj", SearchOption.TopDirectoryOnly))
            {
                System.Xml.Linq.XDocument document;
                try
                {
                    document = System.Xml.Linq.XDocument.Load(project);
                }
                catch (Exception ex) when (ex is System.Xml.XmlException or IOException)
                {
                    continue;
                }

                var directory = Path.GetDirectoryName(project) ?? root;
                foreach (var analyzer in document.Descendants("Analyzer"))
                {
                    var include = analyzer.Attribute("Include")?.Value;
                    if (string.IsNullOrWhiteSpace(include) || include.Contains('*'))
                    {
                        continue;
                    }

                    yield return Path.GetFullPath(Path.Combine(directory, include));
                }
            }
        }
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

        var files = ApplyTfmPreference(seen.ToList());
        files.Sort(StringComparer.Ordinal);
        return files;
    }

    /// <summary>
    /// If exactly one project under the given roots declares the
    /// <c>AssemblyName</c> the caller passed via <c>--assembly-name</c>,
    /// return that project's own directory as the sole effective root instead
    /// of every root the caller supplied. Otherwise return <paramref
    /// name="roots"/> unchanged.
    /// </summary>
    /// <remarks>
    /// <para>
    /// A NuGet package's GitHub source checkout is usually a monorepo, and
    /// nothing above stops <see cref="CollectSourceFiles"/> from walking all
    /// of it: NLog's ships 44 sample console apps under <c>examples/</c>, all
    /// declaring a top-level <c>class Example</c> with the same <c>Main</c>
    /// signature; Polly's repo builds <c>Polly</c>, <c>Polly.Core</c>,
    /// <c>Polly.Extensions</c>, … as separate NuGet packages from sibling
    /// project directories, a few of which carry their own copy of small
    /// internal helper types (<c>CircuitStateController</c>,
    /// <c>TelemetryUtil</c>); and dotnet/runtime libraries such as
    /// <c>System.Text.Json</c> ship a hand-maintained <c>ref/</c>
    /// reference-assembly project beside the real <c>src/</c> implementation,
    /// declaring the identical public surface with <c>throw null;</c> bodies.
    /// None of that is the package <c>--assembly-name</c> names, and walking
    /// it unconditionally is not merely "extra content" in the document — it
    /// hands Roslyn two distinct declarations with the same fully-qualified
    /// name and signature, so <c>DocumentationCommentId</c> (correctly)
    /// assigns them the identical id, which is a real duplicate declaration
    /// downstream, not a lowering bug. This was reached in practice: six
    /// real nuget-corpus packages failed
    /// <c>Lowering::finish</c> with <c>LoweringError::Duplicate</c> for
    /// exactly this reason before this method existed.
    /// </para>
    /// <para>
    /// This is deliberately NOT a directory-name heuristic of the kind
    /// <see cref="ExcludedDirectories"/>'s own remarks forbid — it never
    /// guesses whether a directory "is the library". The producer already
    /// knows the package's name (it is <c>PackageSource::name</c>, forwarded
    /// as <c>--assembly-name</c> on every real invocation); this only asks
    /// each project file, via its own <c>&lt;AssemblyName&gt;</c> (or the
    /// MSBuild default of the project file's own base name), whether it is
    /// the one the caller meant. That is exact evidence, not a guess. A tie
    /// between two projects with the same declared name — the <c>ref/</c> vs
    /// <c>src/</c> shape above — prefers whichever project is NOT under a
    /// path segment literally named <c>ref</c>, because that convention is
    /// unambiguous across the whole dotnet SDK, not specific to any one
    /// package. Any other kind of tie, or no match at all, leaves the roots
    /// untouched: picking between two equally-named real candidates by
    /// fiat would be exactly the mistake this class already declines to
    /// make, and a caller that already scoped <c>--root</c> to a single
    /// project (as this crate's own tests do) will simply find that one
    /// project and narrow to itself — a no-op.
    /// </para>
    /// </remarks>
    private static IReadOnlyList<string> ScopeToMatchingProject(
        IReadOnlyList<string> roots, string? assemblyName)
    {
        if (string.IsNullOrWhiteSpace(assemblyName))
        {
            return roots;
        }

        var candidates = new List<string>();
        foreach (var root in roots)
        {
            if (!Directory.Exists(root))
            {
                continue;
            }

            var full = Path.GetFullPath(root);
            foreach (var csproj in Directory.EnumerateFiles(full, "*.csproj", SearchOption.AllDirectories))
            {
                if (IsExcluded(full, csproj))
                {
                    continue;
                }

                if (string.Equals(ProjectAssemblyName(csproj), assemblyName, StringComparison.OrdinalIgnoreCase))
                {
                    var dir = Path.GetDirectoryName(csproj);
                    if (dir is not null)
                    {
                        candidates.Add(dir);
                    }
                }
            }
        }

        candidates = candidates.Distinct(StringComparer.Ordinal).ToList();

        if (candidates.Count == 1)
        {
            return [candidates[0]];
        }

        if (candidates.Count > 1)
        {
            var nonRef = candidates.Where(d => !HasPathSegment(d, "ref")).ToList();
            if (nonRef.Count == 1)
            {
                return nonRef;
            }
        }

        return roots;
    }

    /// <summary>Whether any path component of <paramref name="dir"/> is exactly <paramref name="segment"/>.</summary>
    private static bool HasPathSegment(string dir, string segment)
    {
        var parts = dir.Split(
            [Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar],
            StringSplitOptions.RemoveEmptyEntries);
        return parts.Any(p => string.Equals(p, segment, StringComparison.OrdinalIgnoreCase));
    }

    /// <summary>
    /// A project's effective <c>AssemblyName</c>: the explicit MSBuild
    /// property if the file declares one, else the MSBuild default — the
    /// project file's own base name.
    /// </summary>
    private static string ProjectAssemblyName(string csprojPath)
    {
        try
        {
            var document = System.Xml.Linq.XDocument.Load(csprojPath);
            var explicitName = document.Descendants("AssemblyName").FirstOrDefault()?.Value?.Trim();
            if (!string.IsNullOrEmpty(explicitName))
            {
                return explicitName;
            }
        }
        catch (Exception ex) when (ex is System.Xml.XmlException or IOException)
        {
            // Fall through to the filename default below — an unparsable
            // project file is not evidence either way, not a fatal error.
        }

        return Path.GetFileNameWithoutExtension(csprojPath);
    }

    /// <summary>Matches a per-TFM source-file variant's trailing tag.</summary>
    /// <remarks>
    /// Anchored to the end of the filename (minus <c>.cs</c>) so only the
    /// file's own trailing dotted segment counts — never an unrelated dotted
    /// component earlier in the name (e.g. <c>Json.Encoder.cs</c> must not
    /// match on <c>Encoder</c>). Covers the three TFM families this repo's
    /// corpus has actually produced: the unified <c>net5</c>+ line, the
    /// legacy <c>netcoreapp</c> line, and <c>netstandard</c>, each with an
    /// optional version number.
    /// </remarks>
    private static readonly Regex TfmSuffix = new(
        @"\.(net\d+(?:\.\d+)?|netstandard\d*(?:\.\d+)?|netcoreapp\d*(?:\.\d+)?)$",
        RegexOptions.IgnoreCase | RegexOptions.Compiled);

    /// <summary>
    /// Ranks a TFM tag by how modern/specific it is — higher wins ties within
    /// <see cref="ApplyTfmPreference"/>. The three bands keep families ordered
    /// relative to each other (the unified <c>net&lt;N&gt;</c> line ranks above
    /// <c>netcoreapp</c>, which ranks above <c>netstandard</c>, mirroring how
    /// close each gets to a modern managed runtime); the version number, where
    /// present, breaks ties within a family.
    /// </summary>
    private static int TfmRank(string tag)
    {
        static double Version(string digits)
        {
            if (digits.Length == 0)
            {
                return 0;
            }

            return digits.Contains('.')
                ? double.Parse(digits, CultureInfo.InvariantCulture)
                : int.Parse(digits, CultureInfo.InvariantCulture);
        }

        var lower = tag.ToLowerInvariant();
        if (lower.StartsWith("netstandard", StringComparison.Ordinal))
        {
            return 0_000 + (int)(Version(lower["netstandard".Length..]) * 10);
        }
        if (lower.StartsWith("netcoreapp", StringComparison.Ordinal))
        {
            return 1_000 + (int)(Version(lower["netcoreapp".Length..]) * 10);
        }
        return 2_000 + (int)(Version(lower["net".Length..]) * 10);
    }

    /// <summary>
    /// Collapses same-directory, same-base per-TFM source variants to one file
    /// each, keeping the <see cref="TfmRank"/>-highest tag — see this class's
    /// remarks for why, and <see cref="TfmSuffix"/> for what counts as a TFM
    /// tag at all.
    /// </summary>
    /// <remarks>
    /// Deliberately conservative: a file's trailing segment must match
    /// <see cref="TfmSuffix"/> to be considered, and a lone TFM-tagged file
    /// with no same-base, same-directory sibling is always kept — dropping it
    /// would silently lose real content the base file never declares.
    /// </remarks>
    private static List<string> ApplyTfmPreference(List<string> files)
    {
        var groups = new Dictionary<(string Dir, string Base), List<(string File, string Tag, int Rank)>>();
        var passthrough = new List<string>();

        foreach (var file in files)
        {
            var dir = Path.GetDirectoryName(file) ?? string.Empty;
            var stem = Path.GetFileNameWithoutExtension(file);
            var match = TfmSuffix.Match(stem);
            if (!match.Success)
            {
                passthrough.Add(file);
                continue;
            }

            var tag = match.Groups[1].Value;
            var baseName = stem[..^(tag.Length + 1)];
            var key = (dir, baseName);
            if (!groups.TryGetValue(key, out var candidates))
            {
                candidates = [];
                groups[key] = candidates;
            }
            candidates.Add((file, tag, TfmRank(tag)));
        }

        var result = new List<string>(passthrough);
        foreach (var candidates in groups.Values)
        {
            if (candidates.Count == 1)
            {
                result.Add(candidates[0].File);
                continue;
            }

            var ordered = candidates.OrderByDescending(c => c.Rank).ToList();
            var kept = ordered[0];
            result.Add(kept.File);

            foreach (var dropped in ordered.Skip(1))
            {
                Console.Error.WriteLine(
                    $"oracle: preferring TFM variant {Path.GetFileName(kept.File)} " +
                    $"({kept.Tag}) over {Path.GetFileName(dropped.File)} ({dropped.Tag})");
            }
        }

        return result;
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
    private static List<string> GlobalUsings(OracleOptions options, IReadOnlyList<string> roots, IReadOnlyList<string>? capturedConfig, DetectedFrameworks frameworks)
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

        if (options.ImplicitUsings && frameworks.WebImplicitUsings)
        {
            foreach (var ns in WebSdkImplicitUsings)
            {
                Add(ns);
            }
        }

        var (included, removed) = ProjectUsings(roots, capturedConfig);

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
        IReadOnlyList<string> roots, IReadOnlyList<string>? capturedConfig)
    {
        var included = new List<string>();
        var removed = new HashSet<string>(StringComparer.Ordinal);
        var visited = new HashSet<string>(StringComparer.Ordinal);
        var documents = capturedConfig is not null
            ? capturedConfig.Select(text => (Path: (string?)null, Text: (string?)text))
            : CandidateProjectFiles(roots).Select(path => (Path: (string?)path, Text: (string?)null));
        foreach (var candidate in documents)
        {
            if (candidate.Path is { } path && !visited.Add(path)) continue;
            System.Xml.Linq.XDocument document;
            try
            {
                if (candidate.Text is { } text)
                {
                    using var input = new StringReader(text);
                    using var reader = System.Xml.XmlReader.Create(input, new System.Xml.XmlReaderSettings
                    {
                        DtdProcessing = System.Xml.DtdProcessing.Prohibit,
                        XmlResolver = null,
                    });
                    document = System.Xml.Linq.XDocument.Load(reader);
                }
                else document = System.Xml.Linq.XDocument.Load(candidate.Path!);
            }
            catch (Exception ex) when (ex is System.Xml.XmlException or IOException)
            {
                if (capturedConfig is not null) throw new OracleFailure("captured config XML is invalid");
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
    private static IEnumerable<string> PreprocessorSymbols(IReadOnlyList<string> extra, bool exact)
    {
        if (exact) return extra;
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

    /// <summary>Namespaces the Web SDK adds to every file when ImplicitUsings is enabled.</summary>
    private static readonly string[] WebSdkImplicitUsings =
    [
        "System.Net.Http.Json",
        "Microsoft.AspNetCore.Builder",
        "Microsoft.AspNetCore.Hosting",
        "Microsoft.AspNetCore.Http",
        "Microsoft.AspNetCore.Routing",
        "Microsoft.Extensions.Configuration",
        "Microsoft.Extensions.DependencyInjection",
        "Microsoft.Extensions.Hosting",
        "Microsoft.Extensions.Logging",
    ];

    /// <summary>Shared frameworks that the project files under the roots ask for.</summary>
    private sealed class DetectedFrameworks
    {
        public static readonly DetectedFrameworks None = new([], [], false);

        public DetectedFrameworks(IReadOnlyList<string> names, IReadOnlyList<string> referenceDirectories, bool webImplicitUsings)
        {
            Names = names;
            ReferenceDirectories = referenceDirectories;
            WebImplicitUsings = webImplicitUsings;
        }

        public IReadOnlyList<string> Names { get; }
        public IReadOnlyList<string> ReferenceDirectories { get; }
        public bool WebImplicitUsings { get; }
    }

    /// <summary>
    /// Finds out from the project files whether the ASP.NET Core shared framework is
    /// in play, and locates its assemblies next to the runtime this oracle runs on.
    /// </summary>
    /// <remarks>
    /// Read as plain XML, never evaluated: a project is a web project when its root
    /// names the Web SDK or it declares a <c>FrameworkReference</c> to
    /// <c>Microsoft.AspNetCore.App</c>. When the shared framework is not installed
    /// beside the runtime nothing is added, and the compilation reports the unresolved
    /// types as it always did; the framework is never guessed from another location.
    /// </remarks>
    private static DetectedFrameworks DetectSharedFrameworks(IReadOnlyList<string> roots)
    {
        var web = false;
        var webImplicitUsings = false;
        foreach (var root in roots)
        {
            if (!Directory.Exists(root)) continue;
            var full = Path.GetFullPath(root);
            foreach (var project in Directory.EnumerateFiles(full, "*.csproj", SearchOption.AllDirectories).OrderBy(p => p, StringComparer.Ordinal))
            {
                if (IsExcluded(full, project)) continue;
                System.Xml.Linq.XDocument document;
                try
                {
                    using var reader = System.Xml.XmlReader.Create(project, new System.Xml.XmlReaderSettings
                    {
                        DtdProcessing = System.Xml.DtdProcessing.Prohibit,
                        XmlResolver = null,
                    });
                    document = System.Xml.Linq.XDocument.Load(reader);
                }
                catch (Exception ex) when (ex is System.Xml.XmlException or IOException or UnauthorizedAccessException)
                {
                    continue;
                }

                var sdk = document.Root?.Attribute("Sdk")?.Value ?? "";
                var isWeb = sdk.StartsWith("Microsoft.NET.Sdk.Web", StringComparison.Ordinal)
                    || document.Descendants("FrameworkReference").Any(item =>
                        string.Equals(item.Attribute("Include")?.Value, "Microsoft.AspNetCore.App", StringComparison.Ordinal));
                if (!isWeb) continue;
                web = true;
                webImplicitUsings |= document.Descendants("ImplicitUsings").Any(item =>
                    item.Value.Trim() is "enable" or "true");
            }
        }

        if (!web) return DetectedFrameworks.None;
        var runtimeDirectory = Path.GetDirectoryName(typeof(object).Assembly.Location);
        var sharedRoot = runtimeDirectory is null ? null : Path.GetDirectoryName(Path.GetDirectoryName(runtimeDirectory));
        var version = runtimeDirectory is null ? null : Path.GetFileName(runtimeDirectory);
        if (sharedRoot is null || string.IsNullOrEmpty(version)) return DetectedFrameworks.None;
        var aspNet = Path.Combine(sharedRoot, "Microsoft.AspNetCore.App", version);
        if (!Directory.Exists(aspNet)) return DetectedFrameworks.None;
        return new DetectedFrameworks(["Microsoft.AspNetCore.App"], [aspNet], webImplicitUsings);
    }

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
    private static (List<MetadataReference> References, IReadOnlyList<CapturedReferenceWitness>? Witness) ReferenceAssemblies(
        IReadOnlyList<string> overrideDirectories, bool capturedSources, IReadOnlyList<string>? supplementalDirectories = null)
    {
        IReadOnlyList<string> directories = overrideDirectories.Count == 0
            ? [Path.GetDirectoryName(typeof(object).Assembly.Location) ?? "", .. (supplementalDirectories ?? [])]
            : overrideDirectories;

        var references = new List<MetadataReference>();
        var witness = capturedSources ? new List<CapturedReferenceWitness>() : null;
        var seen = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        long capturedReferenceBytes = 0;
        const long maxReferenceBytes = 32L * 1024 * 1024;
        const long maxReferenceSetBytes = 256L * 1024 * 1024;
        foreach (var directory in directories)
        {
            if (string.IsNullOrEmpty(directory) || !Directory.Exists(directory))
                throw new OracleFailure("configured reference assembly directory is unavailable");
            var files = capturedSources
                ? Directory.EnumerateFiles(directory, "*.dll").Take(8193).ToArray()
                : Directory.GetFiles(directory, "*.dll");
            if (capturedSources && files.Length > 8192)
                throw new OracleFailure("captured reference set exceeds file limit");
            Array.Sort(files, StringComparer.Ordinal);

            foreach (var file in files)
            {
                var name = Path.GetFileNameWithoutExtension(file);

                // Native host shims are not managed reference assemblies.
                if (name.StartsWith("api-ms-", StringComparison.OrdinalIgnoreCase)
                    || name.StartsWith("clr", StringComparison.OrdinalIgnoreCase)
                    || name.Equals("hostfxr", StringComparison.OrdinalIgnoreCase)
                    || name.Equals("hostpolicy", StringComparison.OrdinalIgnoreCase)
                    || name.Equals("Microsoft.DiaSymReader.Native.amd64", StringComparison.OrdinalIgnoreCase))
                    continue;

                if (!seen.Add(name))
                    throw new OracleFailure($"duplicate reference assembly name: {name}");
                if (capturedSources && seen.Count > 8192)
                    throw new OracleFailure("captured reference set exceeds file limit");

                try
                {
                    if (capturedSources)
                    {
                        // Roslyn must consume exactly the bytes validated here, not lazily
                        // reopen a mutable path after the metadata check.
                        using var input = File.OpenRead(file);
                        using var buffer = new MemoryStream();
                        var chunk = new byte[64 * 1024];
                        int read;
                        while ((read = input.Read(chunk, 0, chunk.Length)) != 0)
                        {
                            if (buffer.Length + read > maxReferenceBytes
                                || capturedReferenceBytes + buffer.Length + read > maxReferenceSetBytes)
                                throw new OracleFailure("captured reference set exceeds limit");
                            buffer.Write(chunk, 0, read);
                        }
                        var bytes = buffer.ToArray();
                        capturedReferenceBytes = checked(capturedReferenceBytes + bytes.LongLength);
                        if (bytes.LongLength > maxReferenceBytes || capturedReferenceBytes > maxReferenceSetBytes)
                            throw new OracleFailure("captured reference set exceeds limit");
                        using var stream = new MemoryStream(bytes, writable: false);
                        using var pe = new PEReader(stream);
                        if (!pe.HasMetadata || !pe.GetMetadataReader().IsAssembly)
                        {
                            if (overrideDirectories.Count != 0)
                                throw new BadImageFormatException("not a managed assembly");
                            continue;
                        }
                        references.Add(MetadataReference.CreateFromImage(bytes));
                        witness!.Add(new CapturedReferenceWitness(name, bytes.LongLength,
                            Convert.ToHexString(SHA256.HashData(bytes)).ToLowerInvariant()));
                    }
                    else
                    {
                        if (overrideDirectories.Count != 0)
                        {
                            using var stream = File.OpenRead(file);
                            using var pe = new PEReader(stream);
                            if (!pe.HasMetadata || !pe.GetMetadataReader().IsAssembly)
                                throw new BadImageFormatException("not a managed assembly");
                        }
                        references.Add(MetadataReference.CreateFromFile(file));
                    }
                }
                catch (Exception ex) when (ex is BadImageFormatException or IOException)
                {
                    if (overrideDirectories.Count != 0)
                        throw new OracleFailure($"configured reference assembly could not be loaded: {name}");
                    Console.Error.WriteLine($"oracle: skipping reference {name}: {ex.Message}");
                }
            }
        }

        return (references, witness);
    }
}
