using System.Text.Json;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;

namespace Nudox.Oracle;

/// <summary>
/// Entry point for the Nudox C# oracle.
/// </summary>
/// <remarks>
/// <para>
/// The oracle turns a directory of C# sources into the single JSON document
/// described by <c>../src/schema.rs</c>. It runs Roslyn's parser and binder and
/// executes only source generators explicitly configured by the selected
/// project; ordinary analyzers are never invoked.
/// </para>
/// <para>
/// <b>Output sink.</b> The document goes to <c>stdout</c> by default so the
/// shared <c>nudox_producer::oracle::run_json</c> helper — which Go and Java
/// already use, and which reads a child process's stdout — can drive this
/// oracle with no bespoke subprocess code. <c>--out FILE</c> selects a file
/// sink instead, for debugging and for callers that would rather not hold the
/// whole document in a pipe buffer. Every diagnostic goes to stderr, so stdout
/// carries JSON and nothing else.
/// </para>
/// </remarks>
internal static class Program
{
    /// <summary>Schema version stamped into the document's <c>format</c> field.</summary>
    private const int SchemaFormat = 1;

    private static int Main(string[] rawArgs)
    {
        OracleOptions options;
        try
        {
            options = OracleOptions.Parse(rawArgs);
        }
        catch (OracleUsageException ex)
        {
            Console.Error.WriteLine($"oracle: {ex.Message}");
            Console.Error.WriteLine();
            Console.Error.WriteLine(OracleOptions.Usage);
            return 2;
        }

        try
        {
            return Run(options);
        }
        catch (OracleFailure ex)
        {
            Console.Error.WriteLine($"oracle: {ex.Message}");
            return 1;
        }
    }

    private static int Run(OracleOptions options)
    {
        var loaded = SourceLoader.Load(options);

        Console.Error.WriteLine(
            $"oracle: {loaded.SourceFileCount} source file(s), "
                + $"{loaded.ReferenceCount} reference(s), assembly '{loaded.AssemblyName}'");

        foreach (var diagnostic in loaded.ReportableDiagnostics)
        {
            Console.Error.WriteLine($"oracle: {diagnostic}");
        }

        // A buffered stream keeps the writer from issuing a syscall per token.
        // `leaveOpen` matters for stdout: closing the console stream would make
        // any later stderr write on process teardown fail.
        using var output = options.OutputPath is { } path
            ? File.Create(path)
            : Console.OpenStandardOutput();

        if (options.AuthorityImage)
        {
            AuthorityImage.Write(loaded, options.SourceBinding!, output);
            return 0;
        }

        var writerOptions = new JsonWriterOptions
        {
            // The document is machine-read by serde_json; indentation would only
            // inflate it. `SkipValidation = false` keeps the writer honest about
            // unbalanced objects, which is exactly the class of bug that would
            // otherwise surface as an opaque serde error on the Rust side.
            Indented = false,
            SkipValidation = false,
        };

        using (var json = new Utf8JsonWriter(output, writerOptions))
        {
            var extractor = new Extractor(loaded, options);
            extractor.WriteExtraction(json, SchemaFormat);
        }

        if (options.OutputPath is { } written)
        {
            Console.Error.WriteLine($"oracle: wrote {written}");
        }

        return 0;
    }
}

/// <summary>Raised for a malformed command line; the caller prints usage.</summary>
internal sealed class OracleUsageException(string message) : Exception(message);

/// <summary>Raised when the oracle cannot produce a document at all.</summary>
internal sealed class OracleFailure(string message) : Exception(message);

/// <summary>The extraction mode: which side of the compiler the facts come from.</summary>
internal enum OracleMode
{
    /// <summary>Parse and bind C# source files under the roots.</summary>
    Source,

    /// <summary>Read an already-compiled assembly's metadata. Not implemented.</summary>
    Metadata,
}

/// <summary>Parsed command line.</summary>
internal sealed record OracleOptions
{
    public required OracleMode Mode { get; init; }

    /// <summary>Directories to collect <c>*.cs</c> from.</summary>
    public required IReadOnlyList<string> Roots { get; init; }

    /// <summary>Where to write the document; <c>null</c> means stdout.</summary>
    public string? OutputPath { get; init; }

    /// <summary>Whether stdout or <c>--out</c> receives a binary authority image.</summary>
    public bool AuthorityImage { get; init; }

    /// <summary>The closed C# language version used by Roslyn's parser.</summary>
    public required LanguageVersion LanguageVersion { get; init; }

    /// <summary>Exact C# source file whose raw bytes bind a binary authority image.</summary>
    public string? SourceBinding { get; init; }

    /// <summary>Overrides the assembly name inferred from the roots.</summary>
    public string? AssemblyName { get; init; }

    /// <summary>Directory of reference assemblies; <c>null</c> means the running runtime's.</summary>
    public string? ReferenceDirectory { get; init; }

    /// <summary>
    /// Extra <c>#define</c> symbols. The target-framework set is always defined;
    /// these are added on top for packages that gate on their own symbols.
    /// </summary>
    public IReadOnlyList<string> DefineSymbols { get; init; } = [];

    /// <summary>Additional namespaces to make globally visible.</summary>
    public IReadOnlyList<string> ExtraUsings { get; init; } = [];

    /// <summary>
    /// Supply the .NET SDK's implicit global usings.
    /// </summary>
    /// <remarks>
    /// On by default because it is the modern SDK default, and because the
    /// failure mode when it is wrong is severe and quiet: a package built with
    /// <c>ImplicitUsings</c> binds <c>TimeSpan</c>, <c>Task</c> and <c>Func</c>
    /// to nothing, so most of its signatures degrade to error types and the
    /// extraction still looks superficially fine.
    /// </remarks>
    public bool ImplicitUsings { get; init; } = true;

    /// <summary>
    /// Emit types and members that are not part of the public surface.
    /// </summary>
    /// <remarks>
    /// On by default: the IR records a <c>Visibility</c> per entry, so a consumer
    /// can filter, whereas an omitted symbol is unrecoverable. Pass
    /// <c>--public-only</c> to trade that richness for a smaller document.
    /// </remarks>
    public bool IncludeNonPublic { get; init; } = true;

    public const string Usage = """
        usage: dotnet oracle.dll --mode source --root DIR [--root DIR ...] [options]

          --mode MODE           'source' (parse and bind .cs files). Required.
          --root DIR            A directory to collect *.cs from. Repeatable, required.
          --out FILE            Write the JSON document here instead of stdout.
          --authority-image     Emit the fixed binary authority image, not JSON.
          --source-binding FILE Bind the authority image to this configured source file.
          --lang-version VER   C# language version (csharp-10 through csharp-14).
          --assembly-name NAME  Override the inferred assembly name.
          --ref-dir DIR         Reference assemblies (default: the running runtime's).
          --define SYM          Extra preprocessor symbol. Repeatable.
          --using NS            Extra global using. Repeatable.
          --no-implicit-usings  Do not supply the SDK's implicit global usings.
          --public-only         Emit only public/protected API surface.

        The JSON document is written to stdout unless --out is given.
        All diagnostics are written to stderr.
        """;

    public static OracleOptions Parse(string[] args)
    {
        OracleMode? mode = null;
        var roots = new List<string>();
        var defines = new List<string>();
        var usings = new List<string>();
        string? outPath = null;
        string? assemblyName = null;
        string? refDir = null;
        var includeNonPublic = true;
        var implicitUsings = true;
        var authorityImage = false;
        string? sourceBinding = null;
        var languageVersion = LanguageVersion.Preview;

        for (var i = 0; i < args.Length; i++)
        {
            var arg = args[i];

            string Value(string flag)
            {
                if (i + 1 >= args.Length)
                {
                    throw new OracleUsageException($"{flag} requires a value");
                }

                return args[++i];
            }

            switch (arg)
            {
                case "--mode":
                    var raw = Value("--mode");
                    mode = raw switch
                    {
                        "source" => OracleMode.Source,
                        "metadata" => OracleMode.Metadata,
                        _ => throw new OracleUsageException(
                            $"unknown mode '{raw}' (expected 'source' or 'metadata')"),
                    };
                    break;
                case "--root":
                    roots.Add(Value("--root"));
                    break;
                case "--out":
                    outPath = Value("--out");
                    break;
                case "--authority-image":
                    authorityImage = true;
                    break;
                case "--lang-version":
                    languageVersion = ParseLanguageVersion(Value("--lang-version"));
                    break;
                case "--source-binding":
                    sourceBinding = Value("--source-binding");
                    break;
                case "--assembly-name":
                    assemblyName = Value("--assembly-name");
                    break;
                case "--ref-dir":
                    refDir = Value("--ref-dir");
                    break;
                case "--define":
                    defines.Add(Value("--define"));
                    break;
                case "--using":
                    usings.Add(Value("--using"));
                    break;
                case "--no-implicit-usings":
                    implicitUsings = false;
                    break;
                case "--public-only":
                    includeNonPublic = false;
                    break;
                case "-h":
                case "--help":
                    throw new OracleUsageException("help requested");
                default:
                    throw new OracleUsageException($"unrecognised argument '{arg}'");
            }
        }

        if (mode is null)
        {
            throw new OracleUsageException("--mode is required");
        }

        if (authorityImage && sourceBinding is null)
        {
            throw new OracleUsageException("--authority-image requires --source-binding FILE");
        }

        if (mode == OracleMode.Metadata)
        {
            // Reading an already-compiled assembly is a genuinely different
            // extraction path (metadata readers, a sidecar XML documentation
            // provider, and real type-forwarder support). Reporting that plainly
            // is better than silently degrading to an empty document, which the
            // Rust side would surface as the misleading `NoTypes`.
            throw new OracleUsageException(
                "--mode metadata is not implemented; only --mode source is supported");
        }

        if (roots.Count == 0)
        {
            throw new OracleUsageException("at least one --root is required");
        }

        return new OracleOptions
        {
            Mode = mode.Value,
            Roots = roots,
            OutputPath = outPath,
            AuthorityImage = authorityImage,
            LanguageVersion = languageVersion,
            SourceBinding = sourceBinding,
            AssemblyName = assemblyName,
            ReferenceDirectory = refDir,
            DefineSymbols = defines,
            ExtraUsings = usings,
            ImplicitUsings = implicitUsings,
            IncludeNonPublic = includeNonPublic,
        };
    }

    private static LanguageVersion ParseLanguageVersion(string raw)
        => raw switch
        {
            "csharp-10" => LanguageVersion.CSharp10,
            "csharp-11" => LanguageVersion.CSharp11,
            "csharp-12" => LanguageVersion.CSharp12,
            "csharp-13" => LanguageVersion.CSharp13,
            "csharp-14" => LanguageVersion.CSharp14,
            _ => throw new OracleUsageException(
                $"unknown C# language version '{raw}' (expected csharp-10 through csharp-14)"),
        };
}
