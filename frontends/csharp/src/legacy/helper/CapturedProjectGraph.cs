using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;

namespace Nudox.Oracle;

internal static partial class SourceLoader
{
    // This is a capture protocol, never an MSBuild evaluation request. All paths
    // in Sources, Config and ProjectReferences are relative to the package root.
    private sealed record CapturedProjectGraph(int Format, string RootProject, CapturedProjectNode[] Projects);
    private sealed record CapturedProjectNode(string ProjectPath, string AssemblyName, string TargetFramework,
        string LanguageVersion, string[] DefineSymbols, string Nullable, bool? AllowUnsafe, string OutputType,
        string[] Usings, CapturedSource[] Sources, CapturedConfig[] Config,
        string[] ReferenceDirectories, string[] ProjectReferences);

    private static bool PortablePath(string? path, string extension) =>
        !string.IsNullOrEmpty(path) && Encoding.UTF8.GetByteCount(path) <= 4096
        && !Path.IsPathRooted(path) && !path.Contains('\\') && !path.Contains(':')
        && !path.Contains('\0') && path.Split('/').All(part => part is not ("" or "." or ".."))
        && path.EndsWith(extension, StringComparison.OrdinalIgnoreCase);

    private static LoadedCompilation LoadCapturedGraph(OracleOptions options, string manifestPath)
    {
        var json = ReadBoundedManifest(manifestPath, 64 * 1024 * 1024);
        CapturedProjectGraph graph;
        try
        {
            graph = JsonSerializer.Deserialize<CapturedProjectGraph>(json, new JsonSerializerOptions
            {
                UnmappedMemberHandling = JsonUnmappedMemberHandling.Disallow,
                MaxDepth = 32,
            }) ?? throw new OracleFailure("empty captured project graph");
        }
        catch (JsonException) { throw new OracleFailure("invalid captured project graph"); }
        if (graph.Format != 1 || graph.Projects is null || graph.Projects.Length is < 1 or > 64
            || !PortablePath(graph.RootProject, ".csproj"))
            throw new OracleFailure("invalid captured project graph header");
        var root = Path.GetFullPath(options.Roots[0]);
        if (options.ProjectFile is null || Path.GetFullPath(options.ProjectFile) != Path.GetFullPath(Path.Combine(root, graph.RootProject)))
            throw new OracleFailure("captured graph root does not match selected project");

        var nodes = new Dictionary<string, CapturedProjectNode>(StringComparer.Ordinal);
        var assemblyNames = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        var bytes = new Dictionary<string, byte[]>(StringComparer.Ordinal);
        long sourceBytes = 0;
        var edgeCount = 0;
        foreach (var node in graph.Projects)
        {
            if (node is null || !PortablePath(node.ProjectPath, ".csproj") || !nodes.TryAdd(node.ProjectPath, node)
                || string.IsNullOrWhiteSpace(node.AssemblyName) || node.AssemblyName.Length > 256
                || node.AssemblyName.IndexOfAny(['/', '\\', ':', '\0']) >= 0 || !assemblyNames.Add(node.AssemblyName)
                || node.TargetFramework is null || node.TargetFramework.Length is < 4 or > 32
                || !node.TargetFramework.StartsWith("net", StringComparison.Ordinal)
                || !node.TargetFramework.AsSpan(3).ToArray().All(c => char.IsAsciiDigit(c) || c == '.')
                || node.Nullable is not ("disable" or "enable" or "warnings" or "annotations")
                || node.AllowUnsafe is null || node.OutputType is not ("Exe" or "Library" or "WinExe")
                || node.DefineSymbols is null || node.DefineSymbols.Length > 256
                || node.DefineSymbols.Any(symbol => symbol is null || symbol.Length > 256 || !SyntaxFacts.IsValidIdentifier(symbol))
                || node.Usings is null || node.Usings.Length > 256 || node.Usings.Any(ns => string.IsNullOrWhiteSpace(ns) || ns.Length > 1024)
                || node.Config is null || node.Sources is null || node.Sources.Length is < 1 or > 8192
                || node.ReferenceDirectories is null || node.ReferenceDirectories.Length is < 1 or > 64
                || node.ReferenceDirectories.Any(path => string.IsNullOrEmpty(path) || path.Length > 4096 || !Path.IsPathFullyQualified(path))
                || node.ProjectReferences is null || node.ProjectReferences.Length > 64
                || node.ProjectReferences.Distinct(StringComparer.Ordinal).Count() != node.ProjectReferences.Length
                || node.ProjectReferences.Any(path => !PortablePath(path, ".csproj")))
                throw new OracleFailure("invalid captured project node");
            if (!LanguageVersionFacts.TryParse(node.LanguageVersion, out var language) || language == LanguageVersion.Default)
                throw new OracleFailure("invalid captured project language version");
            edgeCount += node.ProjectReferences.Length;
            if (edgeCount > 256) throw new OracleFailure("captured project graph exceeds edge limit");
            var prefix = Path.GetDirectoryName(node.ProjectPath)?.Replace('\\', '/');
            foreach (var source in node.Sources)
            {
                if (source is null || !PortablePath(source.Path, ".cs") || source.Text is null
                    || (!string.IsNullOrEmpty(prefix) && !source.Path.StartsWith(prefix + "/", StringComparison.Ordinal)))
                    throw new OracleFailure("captured graph source is outside project");
                var content = Encoding.UTF8.GetBytes(source.Text);
                sourceBytes += content.LongLength;
                if (sourceBytes > 64 * 1024 * 1024 || bytes.Count >= 8192
                    || !bytes.TryAdd(Path.GetFullPath(Path.Combine(root, source.Path)), content))
                    throw new OracleFailure("captured graph source limit or duplicate source");
            }
            // The project and every ancestor presence/absence probe are checked
            // by the same validator as a single captured compilation.
            var configJson = JsonSerializer.Serialize(node.Config);
            if (Encoding.UTF8.GetByteCount(configJson) > 16 * 1024 * 1024)
                throw new OracleFailure("captured graph config exceeds limit");
            ParseCapturedConfig(root, Path.Combine(root, node.ProjectPath), configJson);
        }
        if (!nodes.ContainsKey(graph.RootProject) || nodes.Values.Any(node => node.ProjectReferences.Any(path => !nodes.ContainsKey(path))))
            throw new OracleFailure("captured graph has a missing project reference");

        // Validate the entire closure before reading any reference directory or
        // constructing a compilation. Disconnected nodes and cycles fail closed.
        var visiting = new HashSet<string>(StringComparer.Ordinal);
        var order = new List<string>();
        var visited = new HashSet<string>(StringComparer.Ordinal);
        void Visit(string path, int depth)
        {
            if (depth > 16) throw new OracleFailure("captured project graph exceeds depth limit");
            if (visiting.Contains(path)) throw new OracleFailure("captured project graph contains a cycle");
            if (!visited.Add(path)) return;
            visiting.Add(path);
            foreach (var dependency in nodes[path].ProjectReferences) Visit(dependency, depth + 1);
            visiting.Remove(path);
            order.Add(path);
        }
        Visit(graph.RootProject, 0);
        if (order.Count != nodes.Count) throw new OracleFailure("captured project graph contains unreachable nodes");

        var loaded = new Dictionary<string, LoadedCompilation>(StringComparer.Ordinal);
        long graphReferenceBytes = 0;
        var graphReferences = new Dictionary<string, CapturedReferenceWitness>(StringComparer.OrdinalIgnoreCase);
        foreach (var path in order)
        {
            var node = nodes[path];
            LanguageVersionFacts.TryParse(node.LanguageVersion, out var language);
            var nodeOptions = options with
            {
                CapturedProjectGraphPath = null, CapturedSources = false, CapturedSourcesPath = null, CapturedConfigPath = null,
                ProjectFile = Path.Combine(root, path), AssemblyName = node.AssemblyName,
                TargetFramework = node.TargetFramework, LanguageVersion = language,
                DefineSymbols = node.DefineSymbols, ExactDefineSymbols = true,
                ExactNullable = node.Nullable, ExactUnsafe = node.AllowUnsafe, ExactOutputType = node.OutputType,
                ExtraUsings = node.Usings, ImplicitUsings = false, ReferenceDirectories = node.ReferenceDirectories,
            };
            var sources = node.Sources.ToDictionary(source => Path.GetFullPath(Path.Combine(root, source.Path)),
                source => bytes[Path.GetFullPath(Path.Combine(root, source.Path))], StringComparer.Ordinal);
            var config = ParseCapturedConfig(root, nodeOptions.ProjectFile, JsonSerializer.Serialize(node.Config));
            // Each project keeps its own assembly and parse/compilation options.
            // A CompilationReference retains source declaration identities, unlike
            // emitting and reopening a DLL or combining sibling syntax trees.
            var dependencies = node.ProjectReferences.Select(dependency =>
                (MetadataReference)loaded[dependency].Compilation.ToMetadataReference()).ToArray();
            var item = LoadCore(nodeOptions, sources, config, dependencies);
            foreach (var witness in item.CapturedReferences!)
            {
                if (graphReferences.TryGetValue(witness.Name, out var previous))
                {
                    if (previous != witness) throw new OracleFailure("captured graph has conflicting reference snapshots");
                }
                else
                {
                    graphReferenceBytes += witness.Length;
                    if (graphReferenceBytes > 256L * 1024 * 1024 || graphReferences.Count >= 8192)
                        throw new OracleFailure("captured graph reference set exceeds limit");
                    graphReferences.Add(witness.Name, witness);
                }
            }
            loaded.Add(path, item);
        }
        var result = loaded[graph.RootProject];
        return result with
        {
            CapturedBytes = bytes,
            CapturedConfigDigest = Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes(json))).ToLowerInvariant(),
            CapturedGraphTrees = loaded.Values.SelectMany(item => item.Compilation.SyntaxTrees).ToHashSet(),
            CapturedReferences = graphReferences.Values.OrderBy(item => item.Name, StringComparer.Ordinal).ToArray(),
            ErrorCount = loaded.Values.Sum(item => item.ErrorCount),
            ReportableDiagnostics = loaded.Values.SelectMany(item => item.ReportableDiagnostics).Take(MaxReportedDiagnostics).ToArray(),
        };
    }
}
