using System.Text.Json;
using System.Security.Cryptography;
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
            if (options.CaptureEvidencePath is null)
            {
                AuthorityImage.Write(loaded, options.SourceBinding!, output);
                return 0;
            }
            if (loaded.CapturedBytes is null || loaded.CapturedConfigDigest is null || loaded.CapturedReferences is null
                || loaded.CapturedAcceptedUsings is null || loaded.CapturedDefines is null)
                throw new OracleFailure("captured evidence inputs are unavailable");
            using var image = new MemoryStream();
            var occurrences = AuthorityImage.Write(loaded, options.SourceBinding!, image, captureOccurrences: true);
            if (image.Length > 32 * 1024 * 1024) throw new OracleFailure("captured image exceeds limit");
            var imageBytes = image.ToArray();
            var sources = loaded.CapturedBytes.OrderBy(pair => pair.Key, StringComparer.Ordinal).Select(pair => new
            {
                path = Path.GetRelativePath(loaded.Root, pair.Key).Replace('\\', '/'),
                length = pair.Value.LongLength,
                sha256 = Convert.ToHexString(SHA256.HashData(pair.Value)).ToLowerInvariant(),
            });
            using var evidenceStream = new LimitedEvidenceStream(2 * 1024 * 1024);
            JsonSerializer.Serialize(evidenceStream, new
            {
                format = 2,
                nonce = options.CaptureEvidenceNonce,
                sourceBinding = Path.GetRelativePath(loaded.Root, options.SourceBinding!).Replace('\\', '/'),
                imageSha256 = Convert.ToHexString(SHA256.HashData(imageBytes)).ToLowerInvariant(),
                imageLength = imageBytes.LongLength,
                configSha256 = loaded.CapturedConfigDigest,
                sources,
                references = loaded.CapturedReferences,
                occurrences = occurrences.Select(item => new { owner = item.Owner, start = item.Start, end = item.End, target = item.Target }),
                assembly = loaded.AssemblyName,
                framework = loaded.TargetFramework,
                language = options.LanguageVersion.ToString(),
                outputKind = loaded.Compilation.Options.OutputKind.ToString(),
                defines = loaded.CapturedDefines,
                usings = loaded.CapturedAcceptedUsings,
                generators = "unknown",
                inputClosure = "unknown",
                errors = loaded.ErrorCount,
            });
            var evidence = evidenceStream.ToArray();
            using (var evidenceFile = new FileStream(options.CaptureEvidencePath, FileMode.CreateNew, FileAccess.Write, FileShare.None))
                evidenceFile.Write(evidence);
            output.Write(imageBytes);
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

// Keep evidence serialization bounded while it is written, not only after a
// potentially much larger temporary JSON allocation has already occurred.
internal sealed class LimitedEvidenceStream(long limit) : MemoryStream
{
    private void Check(int count)
    {
        if (count < 0 || Position > limit - count) throw new OracleFailure("captured evidence exceeds limit");
    }
    public override void Write(byte[] buffer, int offset, int count) { Check(count); base.Write(buffer, offset, count); }
    public override void Write(ReadOnlySpan<byte> buffer) { Check(buffer.Length); base.Write(buffer); }
    public override void WriteByte(byte value) { Check(1); base.WriteByte(value); }
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

    /// <summary>Read exact captured source members from standard input.</summary>
    public bool CapturedSources { get; init; }
    public string? CapturedSourcesPath { get; init; }
    public string? CapturedConfigPath { get; init; }
    public string? CaptureEvidencePath { get; init; }
    public string? CaptureEvidenceNonce { get; init; }

    /// <summary>Overrides the assembly name inferred from the roots.</summary>
    public string? AssemblyName { get; init; }

    /// <summary>Exact project file whose directory scopes source loading.</summary>
    public string? ProjectFile { get; init; }

    /// <summary>Directory of reference assemblies; <c>null</c> means the running runtime's.</summary>
    public IReadOnlyList<string> ReferenceDirectories { get; init; } = [];

    /// <summary>
    /// Extra <c>#define</c> symbols. The target-framework set is always defined;
    /// these are added on top for packages that gate on their own symbols.
    /// </summary>
    public IReadOnlyList<string> DefineSymbols { get; init; } = [];
    /// <summary>Use only the explicitly supplied captured SDK defines.</summary>
    public bool ExactDefineSymbols { get; init; }
    /// <summary>Captured SDK target framework, if explicitly observed.</summary>
    public string? TargetFramework { get; init; }
    /// <summary>Captured nullable and unsafe compiler options, if observed.</summary>
    public string? ExactNullable { get; init; }
    public bool? ExactUnsafe { get; init; }
    /// <summary>Captured SDK output type, when observed.</summary>
    public string? ExactOutputType { get; init; }

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
          --captured-sources   Read a bounded JSON source manifest from standard input.
          --captured-sources-file FILE Read a bounded captured source manifest from FILE.
          --captured-config-file FILE Read bounded project and ancestor probes from FILE.
          --lang-version VER   C# language version (csharp-10 through csharp-14).
          --assembly-name NAME  Override the inferred assembly name.
          --project-file FILE  Select this exact project beneath the package root.
          --ref-dir DIR         Reference assembly directory; repeat for multiple packs.
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
        string? targetFramework = null;
        string? exactNullable = null;
        bool? exactUnsafe = null;
        string? exactOutputType = null;
        string? projectFile = null;
        var refDirs = new List<string>();
        var includeNonPublic = true;
        var implicitUsings = true;
        var exactDefines = false;
        var authorityImage = false;
        var capturedSources = false;
        string? capturedSourcesPath = null;
        string? capturedConfigPath = null;
        string? captureEvidencePath = null;
        string? captureEvidenceNonce = null;
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
                case "--captured-sources":
                    capturedSources = true;
                    break;
                case "--captured-sources-file":
                    capturedSourcesPath = Value("--captured-sources-file");
                    break;
                case "--captured-config-file":
                    capturedConfigPath = Value("--captured-config-file");
                    break;
                case "--capture-evidence-file":
                    captureEvidencePath = Value("--capture-evidence-file");
                    break;
                case "--capture-evidence-nonce":
                    captureEvidenceNonce = Value("--capture-evidence-nonce");
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
                case "--target-framework":
                    targetFramework = Value("--target-framework");
                    break;
                case "--exact-output-type":
                    exactOutputType = Value("--exact-output-type");
                    break;
                case "--exact-nullable":
                    exactNullable = Value("--exact-nullable");
                    break;
                case "--exact-unsafe":
                    var unsafeValue = Value("--exact-unsafe");
                    exactUnsafe = unsafeValue switch { "true" => true, "false" => false, _ => throw new OracleUsageException("invalid exact unsafe option") };
                    break;
                case "--project-file":
                    projectFile = Value("--project-file");
                    break;
                case "--ref-dir":
                    refDirs.Add(Value("--ref-dir"));
                    break;
                case "--define":
                    defines.Add(Value("--define"));
                    break;
                case "--exact-defines":
                    exactDefines = true;
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
        if (capturedSources && capturedSourcesPath is not null)
            throw new OracleUsageException("choose one captured source manifest input");
        if (capturedConfigPath is not null && ((!capturedSources && capturedSourcesPath is null) || projectFile is null))
            throw new OracleUsageException("captured configuration requires captured sources and exact project");
        if (exactDefines && (capturedConfigPath is null || (!capturedSources && capturedSourcesPath is null)))
            throw new OracleUsageException("exact defines require captured source and configuration");
        if (targetFramework is not null && (capturedConfigPath is null || (!capturedSources && capturedSourcesPath is null)
            || targetFramework.Length is < 4 or > 32 || !targetFramework.StartsWith("net", StringComparison.Ordinal)
            || !targetFramework.AsSpan(3).ToArray().All(c => char.IsAsciiDigit(c) || c == '.')))
            throw new OracleUsageException("target framework requires captured inputs and a simple net target");
        if ((exactNullable is not null || exactUnsafe is not null) && (capturedConfigPath is null
            || (!capturedSources && capturedSourcesPath is null) || exactNullable is not ("disable" or "enable" or "warnings" or "annotations") || exactUnsafe is null))
            throw new OracleUsageException("exact nullable and unsafe require captured inputs and valid values");
        if (exactOutputType is not null && (capturedConfigPath is null || (!capturedSources && capturedSourcesPath is null)
            || exactOutputType is not ("Exe" or "Library" or "WinExe")))
            throw new OracleUsageException("exact output type requires captured inputs and a valid output type");
        if ((captureEvidencePath is null) != (captureEvidenceNonce is null)
            || (captureEvidencePath is not null && (!authorityImage || capturedConfigPath is null
                || captureEvidenceNonce!.Length != 32 || !captureEvidenceNonce.All(c => c is >= '0' and <= '9' or >= 'a' and <= 'f'))))
            throw new OracleUsageException("captured evidence requires an image, captured inputs, and a lowercase nonce");

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
            CapturedSources = capturedSources,
            CapturedSourcesPath = capturedSourcesPath,
            CapturedConfigPath = capturedConfigPath,
            CaptureEvidencePath = captureEvidencePath,
            CaptureEvidenceNonce = captureEvidenceNonce,
            AssemblyName = assemblyName,
            ProjectFile = projectFile,
            ReferenceDirectories = refDirs,
            DefineSymbols = defines,
            ExactDefineSymbols = exactDefines,
            TargetFramework = targetFramework,
            ExactNullable = exactNullable,
            ExactUnsafe = exactUnsafe,
            ExactOutputType = exactOutputType,
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
