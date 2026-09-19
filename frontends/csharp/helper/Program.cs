using System.Buffers.Binary;
using System.Text;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;

namespace Nudox.Oracle;

/// <summary>Raised when the checked request cannot produce a semantic image.</summary>
internal sealed class OracleFailure(string message) : Exception(message);

/// <summary>The only source mode currently admitted by the native boundary.</summary>
internal enum OracleMode
{
    Source,
}

/// <summary>Options shared with the full Roslyn source loader.</summary>
internal sealed record OracleOptions
{
    public required OracleMode Mode { get; init; }
    public required IReadOnlyList<string> Roots { get; init; }
    public string? AssemblyName { get; init; }
    public string? ReferenceDirectory { get; init; }
    public IReadOnlyList<string> DefineSymbols { get; init; } = [];
    public IReadOnlyList<string> ExtraUsings { get; init; } = [];
    public bool ImplicitUsings { get; init; } = true;
    public bool IncludeNonPublic { get; init; } = true;
}

/// <summary>
/// Native C# authority entry point. It consumes the checked BCQ request and
/// emits only the checked BCN semantic envelope; compiler chatter stays on
/// stderr and can never be mistaken for a fact.
/// </summary>
internal static class Program
{
    public static int Main()
    {
        string? root = null;
        try
        {
            var request = Request.Read(Console.OpenStandardInput());
            if (request.Language != "csharp")
            {
                throw new InvalidDataException("request language is not csharp");
            }

            root = Directory.CreateTempSubdirectory("backend-csharp-authority-").FullName;
            MaterializeSources(request, root);
            var options = Options(request, root);
            var loaded = SourceLoader.Load(options);
            var sourcePath = Path.Combine(root, "source.cs");
            var output = NativeEmitter.Emit(loaded, request, sourcePath);
            Envelope.Write(Console.OpenStandardOutput(), request, output.Records, output.Complete);
            return 0;
        }
        catch (Exception error) when (
            error is IOException
                or UnauthorizedAccessException
                or InvalidDataException
                or InvalidOperationException
                or ArgumentException
                or OverflowException)
        {
            Console.Error.WriteLine($"csharp authority: {error.Message}");
            return 1;
        }
        finally
        {
            if (root is not null)
            {
                try
                {
                    Directory.Delete(root, recursive: true);
                }
                catch (IOException error)
                {
                    Console.Error.WriteLine($"csharp authority: cleanup failed: {error.Message}");
                }
            }
        }
    }

    private static void MaterializeSources(Request request, string root)
    {
        var sourceCount = 0;
        foreach (var pair in request.Inputs)
        {
            if (!pair.Key.EndsWith(".cs", StringComparison.OrdinalIgnoreCase))
            {
                continue;
            }

            var relative = pair.Key.Replace('\\', '/');
            if (relative.StartsWith('/') || relative.Split('/').Any(part => part is "" or "." or ".."))
            {
                throw new InvalidDataException($"source input escapes authority root: {pair.Key}");
            }

            var path = Path.GetFullPath(Path.Combine(root, relative));
            if (!path.StartsWith(Path.GetFullPath(root) + Path.DirectorySeparatorChar, StringComparison.Ordinal))
            {
                throw new InvalidDataException($"source input escapes authority root: {pair.Key}");
            }

            Directory.CreateDirectory(Path.GetDirectoryName(path)!);
            File.WriteAllBytes(path, pair.Value);
            sourceCount++;
        }

        if (sourceCount == 0 || !File.Exists(Path.Combine(root, "source.cs")))
        {
            throw new InvalidDataException("BCQ request has no source.cs input");
        }
    }

    private static OracleOptions Options(Request request, string root)
    {
        var config = request.Inputs.TryGetValue("roslyn-options", out var bytes)
            ? Encoding.UTF8.GetString(bytes)
            : string.Empty;
        var assemblyName = "BackendSemantic";
        string? referenceDirectory = null;
        var defines = new List<string>();
        var usings = new List<string>();
        var implicitUsings = true;

        // The frontend owns the opaque configuration input. These explicit
        // spellings make the native command useful to callers while leaving
        // unknown settings visible in the manifest rather than guessing them.
        foreach (var token in config.Split([';', '\n', '\r'], StringSplitOptions.RemoveEmptyEntries))
        {
            var value = token.Trim();
            if (value.StartsWith("assembly=", StringComparison.OrdinalIgnoreCase))
            {
                assemblyName = value["assembly=".Length..].Trim();
            }
            else if (value.StartsWith("ref-dir=", StringComparison.OrdinalIgnoreCase))
            {
                referenceDirectory = value["ref-dir=".Length..].Trim();
            }
            else if (value.StartsWith("define=", StringComparison.OrdinalIgnoreCase))
            {
                defines.Add(value["define=".Length..].Trim());
            }
            else if (value.StartsWith("using=", StringComparison.OrdinalIgnoreCase))
            {
                usings.Add(value["using=".Length..].Trim());
            }
            else if (value.Equals("no-implicit-usings", StringComparison.OrdinalIgnoreCase))
            {
                implicitUsings = false;
            }
        }

        return new OracleOptions
        {
            Mode = OracleMode.Source,
            Roots = [root],
            AssemblyName = string.IsNullOrWhiteSpace(assemblyName) ? "BackendSemantic" : assemblyName,
            ReferenceDirectory = string.IsNullOrWhiteSpace(referenceDirectory) ? null : referenceDirectory,
            DefineSymbols = defines,
            ExtraUsings = usings,
            ImplicitUsings = implicitUsings,
            IncludeNonPublic = true,
        };
    }
}

/// <summary>One exact BCQ request decoded before Roslyn is entered.</summary>
internal sealed record Request(
    string Language,
    byte[] Session,
    byte[] Manifest,
    byte[] Authority,
    IReadOnlyDictionary<string, byte[]> Inputs)
{
    public static Request Read(Stream input)
    {
        using var memory = new MemoryStream();
        input.CopyTo(memory);
        var cursor = new Cursor(memory.ToArray());
        if (!cursor.Take(4).SequenceEqual("BCQ\0"u8.ToArray()) || cursor.U16() != 1)
        {
            throw new InvalidDataException("bad BCQ request");
        }

        var language = Encoding.UTF8.GetString(cursor.Take(cursor.U16()));
        var count = cursor.U16();
        if (count > 256)
        {
            throw new InvalidDataException("too many BCQ input fields");
        }
        var session = cursor.Take(32);
        var manifest = cursor.Take(32);
        var authority = cursor.Take(32);
        var fields = new SortedDictionary<string, byte[]>(StringComparer.Ordinal);
        for (var i = 0; i < count; i++)
        {
            var nameLength = cursor.U16();
            var valueLength = cursor.U32();
            var name = Encoding.UTF8.GetString(cursor.Take(nameLength));
            if (!fields.TryAdd(name, cursor.Take(valueLength)))
            {
                throw new InvalidDataException($"duplicate BCQ input: {name}");
            }
        }

        if (!cursor.Done)
        {
            throw new InvalidDataException("trailing BCQ bytes");
        }

        return new Request(language, session, manifest, authority, fields);
    }
}

/// <summary>Bounded big-endian cursor for the native request.</summary>
internal sealed class Cursor(byte[] bytes)
{
    private int position;

    public byte[] Take(int length)
    {
        if (length < 0 || position > bytes.Length - length)
        {
            throw new EndOfStreamException("truncated BCQ request");
        }

        var value = bytes[position..(position + length)];
        position += length;
        return value;
    }

    public ushort U16() => BinaryPrimitives.ReadUInt16BigEndian(Take(2));

    public int U32()
    {
        var value = BinaryPrimitives.ReadUInt32BigEndian(Take(4));
        return checked((int)value);
    }

    public bool Done => position == bytes.Length;
}

/// <summary>A flat semantic row before the canonical envelope is encoded.</summary>
internal readonly record struct Record(byte Kind, string Key, string Value);

/// <summary>Result of one Roslyn walk, including an explicit truncation claim.</summary>
internal sealed record NativeOutput(IReadOnlyList<Record> Records, bool Complete);

/// <summary>Roslyn semantic projection used by the native BCQ/BCN bridge.</summary>
internal static class NativeEmitter
{
    private const string Schema = "csharp-semantic-v1";
    private const int MaximumRecords = 4096;
    private const int MaximumValueBytes = 60 * 1024;
    private static readonly SymbolDisplayFormat SignatureFormat =
        SymbolDisplayFormat.FullyQualifiedFormat.WithMiscellaneousOptions(
            SymbolDisplayMiscellaneousOptions.IncludeNullableReferenceTypeModifier);

    public static NativeOutput Emit(LoadedCompilation loaded, Request request, string sourcePath)
    {
        var rows = new List<Record>();
        var seen = new HashSet<string>(StringComparer.Ordinal);
        var trees = loaded.Compilation.SyntaxTrees.OrderBy(tree => tree.FilePath, StringComparer.Ordinal).ToArray();

        foreach (var tree in trees)
        {
            if (tree.FilePath == "<implicit-global-usings>.cs")
            {
                continue;
            }

            var model = loaded.Compilation.GetSemanticModel(tree);
            var offsets = ByteOffsets(tree);
            foreach (var node in tree.GetRoot().DescendantNodesAndSelf())
            {
                foreach (var symbol in DeclaredSymbols(model, node))
                {
                    AddDeclaration(rows, seen, symbol, tree, node, offsets);
                }
            }

            AddReferences(rows, seen, model, tree, offsets);
            AddDiagnostics(
                rows,
                seen,
                loaded.Compilation.GetDiagnostics().Where(diagnostic => diagnostic.Location.SourceTree == tree),
                offsets);
        }

        AddDependencies(rows, seen, loaded);
        Add(rows, seen, 6, "package:reference-assemblies/optional",
            Value("package", "reference-assemblies/optional", "absent"));

        rows.Sort(static (left, right) =>
        {
            var kind = left.Kind.CompareTo(right.Kind);
            return kind != 0 ? kind : StringComparer.Ordinal.Compare(left.Key, right.Key);
        });

        var complete = rows.Count <= MaximumRecords;
        if (!complete)
        {
            rows = rows.Take(MaximumRecords).ToList();
        }

        return new NativeOutput(rows, complete);
    }

    private static IEnumerable<ISymbol> DeclaredSymbols(SemanticModel model, SyntaxNode node)
    {
        if (node is BaseFieldDeclarationSyntax field)
        {
            foreach (var variable in field.Declaration.Variables)
            {
                var symbol = model.GetDeclaredSymbol(variable);
                if (symbol is not null) yield return symbol;
            }

            yield break;
        }

        var symbolForNode = node switch
        {
            BaseNamespaceDeclarationSyntax => model.GetDeclaredSymbol(node),
            BaseTypeDeclarationSyntax => model.GetDeclaredSymbol(node),
            DelegateDeclarationSyntax => model.GetDeclaredSymbol(node),
            MethodDeclarationSyntax => model.GetDeclaredSymbol(node),
            ConstructorDeclarationSyntax => model.GetDeclaredSymbol(node),
            DestructorDeclarationSyntax => model.GetDeclaredSymbol(node),
            PropertyDeclarationSyntax => model.GetDeclaredSymbol(node),
            IndexerDeclarationSyntax => model.GetDeclaredSymbol(node),
            EventDeclarationSyntax => model.GetDeclaredSymbol(node),
            EnumMemberDeclarationSyntax => model.GetDeclaredSymbol(node),
            ParameterSyntax => model.GetDeclaredSymbol(node),
            TypeParameterSyntax => model.GetDeclaredSymbol(node),
            _ => null,
        };
        if (symbolForNode is not null)
        {
            yield return symbolForNode;
        }
    }

    private static void AddDeclaration(
        List<Record> rows,
        HashSet<string> seen,
        ISymbol symbol,
        SyntaxTree tree,
        SyntaxNode node,
        int[] offsets)
    {
        if (symbol.IsImplicitlyDeclared || !symbol.Locations.Any(location => location.IsInSource && location.SourceTree == tree))
        {
            return;
        }

        var id = SymbolId(symbol);
        var owner = symbol.ContainingSymbol is null ? "global" : SymbolId(symbol.ContainingSymbol);
        var signature = Display(symbol);
        var docs = DocComments.For(symbol).Xml ?? string.Empty;
        var span = node.Span;
        var location = $"{Path.GetFileName(tree.FilePath)}:{ByteOffset(offsets, span.Start)}:{ByteOffset(offsets, span.End)}";
        Add(rows, seen, 1, id,
            Value(symbol.Kind.ToString().ToLowerInvariant(), owner, signature, docs + "\u0001" + location));

        var type = DeclaredType(symbol);
        if (type is not null)
        {
            Add(rows, seen, 2, id, Value(id, Display(type)));
        }
    }

    private static void AddReferences(
        List<Record> rows,
        HashSet<string> seen,
        SemanticModel model,
        SyntaxTree tree,
        int[] offsets)
    {
        foreach (var node in tree.GetRoot().DescendantNodes())
        {
            var spelling = node switch
            {
                InvocationExpressionSyntax invocation => invocation.Expression,
                ObjectCreationExpressionSyntax creation => creation.Type,
                MemberAccessExpressionSyntax access => access.Name,
                IdentifierNameSyntax identifier when identifier.Parent is not MemberAccessExpressionSyntax => identifier,
                GenericNameSyntax generic => generic,
                _ => null,
            };
            if (spelling is null)
            {
                continue;
            }

            var target = model.GetSymbolInfo(node).Symbol
                ?? model.GetSymbolInfo(spelling).Symbol
                ?? model.GetSymbolInfo(node).CandidateSymbols.FirstOrDefault();
            var owner = model.GetEnclosingSymbol(node.SpanStart);
            if (owner is null || owner.IsImplicitlyDeclared)
            {
                continue;
            }

            var ownerId = SymbolId(owner);
            var targetId = target is null
                ? "unresolved:" + Clean(spelling.ToString())
                : SymbolId(target);
            var start = ByteOffset(offsets, spelling.SpanStart);
            var end = ByteOffset(offsets, spelling.Span.End);
            Add(rows, seen, 3, $"{ownerId}->{targetId}@{start}:{end}",
                Value(ownerId, targetId, start.ToString(), end.ToString()));
        }
    }

    private static void AddDiagnostics(
        List<Record> rows,
        HashSet<string> seen,
        IEnumerable<Diagnostic> diagnostics,
        int[] offsets)
    {
        foreach (var diagnostic in diagnostics)
        {
            var span = diagnostic.Location.IsInSource ? diagnostic.Location.SourceSpan : default;
            var start = ByteOffset(offsets, span.Start);
            var end = ByteOffset(offsets, span.End);
            var severity = diagnostic.Severity.ToString().ToLowerInvariant();
            Add(rows, seen, 4, $"{diagnostic.Id}@{start}:{end}",
                Value(severity, diagnostic.Id, start.ToString(), end.ToString(), diagnostic.GetMessage()));
        }
    }

    private static void AddDependencies(List<Record> rows, HashSet<string> seen, LoadedCompilation loaded)
    {
        Add(rows, seen, 5, $"package:{loaded.AssemblyName}",
            Value("package", loaded.AssemblyName, "present"));

        foreach (var reference in loaded.Compilation.References)
        {
            var symbol = loaded.Compilation.GetAssemblyOrModuleSymbol(reference);
            var identity = symbol switch
            {
                IAssemblySymbol assembly => assembly.Identity.ToString(),
                IModuleSymbol module => module.ContainingAssembly?.Identity.ToString() ?? module.Name,
                _ => reference.Display ?? "unknown",
            };
            var name = symbol switch
            {
                IAssemblySymbol assembly => assembly.Identity.Name,
                IModuleSymbol module => module.ContainingAssembly?.Identity.Name ?? module.Name,
                _ => reference.Display ?? "unknown",
            };
            Add(rows, seen, 5, $"package:{name}", Value("package", identity, "present"));
        }
    }

    private static ITypeSymbol? DeclaredType(ISymbol symbol) => symbol switch
    {
        IMethodSymbol method => method.ReturnsVoid ? null : method.ReturnType,
        IPropertySymbol property => property.Type,
        IFieldSymbol field => field.Type,
        IEventSymbol @event => @event.Type,
        IParameterSymbol parameter => parameter.Type,
        INamedTypeSymbol type => type,
        _ => null,
    };

    private static string SymbolId(ISymbol symbol)
    {
        var id = DocumentationCommentId.CreateDeclarationId(symbol);
        return Clean(string.IsNullOrWhiteSpace(id)
            ? symbol.ToDisplayString(SymbolDisplayFormat.CSharpErrorMessageFormat)
            : id);
    }

    private static string Display(ISymbol symbol) => Clean(symbol.ToDisplayString(SignatureFormat));

    private static int[] ByteOffsets(SyntaxTree tree)
    {
        var text = tree.GetText().ToString();
        var offsets = new int[text.Length + 1];
        var bytes = 0;
        try
        {
            var raw = File.ReadAllBytes(tree.FilePath);
            if (raw.Length >= 3 && raw[0] == 0xef && raw[1] == 0xbb && raw[2] == 0xbf)
            {
                bytes = 3;
            }
        }
        catch (IOException or ArgumentException) { }

        for (var index = 0; index < text.Length;)
        {
            offsets[index] = bytes;
            var width = char.IsSurrogatePair(text, index) ? 2 : 1;
            if (width == 2) offsets[index + 1] = bytes;
            // Roslyn can preserve an unpaired UTF-16 surrogate in malformed
            // source. Keep the coordinate map total and deterministic: the
            // UTF-8 encoder's replacement fallback is three bytes.
            bytes += width == 2
                ? 4
                : char.IsSurrogate(text[index]) ? 3 : Encoding.UTF8.GetByteCount(text.AsSpan(index, 1));
            index += width;
        }

        offsets[text.Length] = bytes;
        return offsets;
    }

    private static int ByteOffset(int[] offsets, int position) =>
        offsets[Math.Clamp(position, 0, offsets.Length - 1)];

    private static void Add(List<Record> rows, HashSet<string> seen, byte kind, string key, string value)
    {
        key = Clean(key);
        var unique = $"{kind}\u0000{key}";
        if (!seen.Add(unique)) return;
        rows.Add(new Record(kind, key, Clamp(value)));
    }

    private static string Value(params string[] fields) =>
        string.Join('\0', new[] { Schema }.Concat(fields.Select(Clean)));

    private static string Clean(string value) => value.Replace('\0', '\ufffd');

    private static string Clamp(string value)
    {
        var bytes = Encoding.UTF8.GetBytes(value);
        if (bytes.Length <= MaximumValueBytes) return value;
        return Encoding.UTF8.GetString(bytes, 0, MaximumValueBytes - 3) + "...";
    }
}

/// <summary>Canonical big-endian BCN response writer.</summary>
internal static class Envelope
{
    public static void Write(Stream output, Request request, IEnumerable<Record> source, bool complete)
    {
        var records = source
            .OrderBy(record => record.Kind)
            .ThenBy(record => record.Key, StringComparer.Ordinal)
            .GroupBy(record => (record.Kind, record.Key))
            .Select(group => group.First())
            .ToArray();

        using var writer = new BinaryWriter(output, Encoding.UTF8, leaveOpen: true);
        writer.Write("BCN\0"u8.ToArray());
        WriteU16(writer, 1);
        writer.Write(complete ? (byte)0 : (byte)1);
        writer.Write((byte)0);
        WriteU32(writer, records.Length);
        writer.Write(request.Session);
        writer.Write(request.Manifest);
        writer.Write(request.Authority);
        writer.Write(new byte[8]);
        var language = Encoding.UTF8.GetBytes(request.Language);
        WriteU16(writer, language.Length);
        writer.Write(language);

        foreach (var record in records)
        {
            var key = Encoding.UTF8.GetBytes(record.Key);
            var value = Encoding.UTF8.GetBytes(record.Value);
            writer.Write(record.Kind);
            writer.Write((byte)0);
            WriteU32(writer, key.Length);
            WriteU32(writer, value.Length);
            writer.Write(key);
            writer.Write(value);
        }
        writer.Flush();
    }

    private static void WriteU16(BinaryWriter writer, int value) =>
        writer.Write(new[] { (byte)(value >> 8), (byte)value });

    private static void WriteU32(BinaryWriter writer, int value) =>
        writer.Write(new[] { (byte)(value >> 24), (byte)(value >> 16), (byte)(value >> 8), (byte)value });
}
