using System.Buffers.Binary;
using System.Security.Cryptography;
using System.Text;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp.Syntax;

namespace Nudox.Oracle;

/// <summary>Writes source-bound Roslyn declarations as a fixed binary authority image.</summary>
internal static class AuthorityImage
{
    private const int HeaderBytes = 88;
    private const int DeclarationBytes = 20;
    private static readonly byte[] DigestDomain = "nudox.csharp.authority.image.sha256.v1\0"u8.ToArray();

    /// <summary>Emits every source declaration from the configured binding file.</summary>
    public static void Write(LoadedCompilation loaded, string sourceBinding, Stream destination)
    {
        var binding = Path.GetFullPath(sourceBinding);
        var tree = BindingTree(loaded.Compilation, binding);
        var source = File.ReadAllBytes(binding);
        var preamble = Utf8Preamble(source);
        var declarations = CollectDeclarations(loaded.Compilation, tree, preamble);
        var atoms = new List<byte>();
        var rows = new List<Row>(declarations.Count);
        foreach (var declaration in declarations)
        {
            var name = Encoding.UTF8.GetBytes(declaration.Symbol.Name);
            if (name.Length == 0)
            {
                throw new OracleFailure("Roslyn emitted an empty declaration name");
            }
            if ((ulong)atoms.Count > uint.MaxValue || (ulong)name.Length > uint.MaxValue)
            {
                throw new OracleFailure("C# authority atom plane exceeds u32 capacity");
            }
            rows.Add(new Row(
                Kind(declaration.Symbol),
                (uint)atoms.Count,
                (uint)name.Length,
                declaration.Start,
                declaration.End));
            atoms.AddRange(name);
        }
        if ((ulong)rows.Count > uint.MaxValue || (ulong)atoms.Count > uint.MaxValue)
        {
            throw new OracleFailure("C# authority image exceeds u32 capacity");
        }
        var declarationBytes = checked(rows.Count * DeclarationBytes);
        var bodyBytes = checked(declarationBytes + atoms.Count);
        if ((ulong)bodyBytes > uint.MaxValue)
        {
            throw new OracleFailure("C# authority image body exceeds u32 capacity");
        }
        var image = new byte[checked(HeaderBytes + bodyBytes)];
        "NCAI"u8.CopyTo(image);
        BinaryPrimitives.WriteUInt16LittleEndian(image.AsSpan(4, 2), 1);
        BinaryPrimitives.WriteUInt16LittleEndian(image.AsSpan(6, 2), HeaderBytes);
        BinaryPrimitives.WriteUInt32LittleEndian(image.AsSpan(8, 4), (uint)rows.Count);
        BinaryPrimitives.WriteUInt32LittleEndian(image.AsSpan(12, 4), (uint)atoms.Count);
        BinaryPrimitives.WriteUInt32LittleEndian(image.AsSpan(16, 4), (uint)bodyBytes);
        SHA256.HashData(source).CopyTo(image, 20);
        for (var index = 0; index < rows.Count; index++)
        {
            var row = rows[index];
            var start = HeaderBytes + index * DeclarationBytes;
            image[start] = row.Kind;
            BinaryPrimitives.WriteUInt32LittleEndian(image.AsSpan(start + 4, 4), row.NameOffset);
            BinaryPrimitives.WriteUInt32LittleEndian(image.AsSpan(start + 8, 4), row.NameLength);
            BinaryPrimitives.WriteUInt32LittleEndian(image.AsSpan(start + 12, 4), row.Start);
            BinaryPrimitives.WriteUInt32LittleEndian(image.AsSpan(start + 16, 4), row.End);
        }
        atoms.CopyTo(image, HeaderBytes + declarationBytes);
        using var digest = IncrementalHash.CreateHash(HashAlgorithmName.SHA256);
        digest.AppendData(DigestDomain);
        digest.AppendData(image, 0, 52);
        digest.AppendData(image, 84, HeaderBytes - 84);
        digest.AppendData(image, HeaderBytes, bodyBytes);
        digest.GetHashAndReset().CopyTo(image, 52);
        destination.Write(image);
    }

    private static SyntaxTree BindingTree(CSharpCompilation compilation, string binding)
    {
        SyntaxTree? found = null;
        foreach (var tree in compilation.SyntaxTrees)
        {
            if (!StringComparer.Ordinal.Equals(Path.GetFullPath(tree.FilePath), binding))
            {
                continue;
            }
            if (found is not null)
            {
                throw new OracleFailure("C# authority source binding selected multiple syntax trees");
            }
            found = tree;
        }
        return found ?? throw new OracleFailure("C# authority source binding is absent from the Roslyn compilation");
    }

    private static List<BoundDeclaration> CollectDeclarations(
        CSharpCompilation compilation, SyntaxTree tree, int preamble)
    {
        var model = compilation.GetSemanticModel(tree);
        var declarations = new List<BoundDeclaration>();
        foreach (var node in tree.GetRoot().DescendantNodes())
        {
            INamedTypeSymbol? symbol = node switch
            {
                BaseTypeDeclarationSyntax declaration => model.GetDeclaredSymbol(declaration),
                DelegateDeclarationSyntax declaration => model.GetDeclaredSymbol(declaration),
                _ => null,
            };
            if (symbol is null)
            {
                continue;
            }
            var location = symbol.Locations.FirstOrDefault(location => location.IsInSource && location.SourceTree == tree);
            if (location is null)
            {
                throw new OracleFailure("Roslyn declaration omitted its source location");
            }
            var span = location.SourceSpan;
            var text = tree.GetText();
            var start = checked(preamble + Encoding.UTF8.GetByteCount(text.ToString(0, span.Start)));
            var end = checked(start + Encoding.UTF8.GetByteCount(text.ToString(span)));
            declarations.Add(new BoundDeclaration(symbol, checked((uint)start), checked((uint)end)));
        }
        return declarations;
    }

    private static int Utf8Preamble(byte[] source)
    {
        var preamble = new UTF8Encoding(encoderShouldEmitUTF8Identifier: true).GetPreamble();
        return source.AsSpan().StartsWith(preamble) ? preamble.Length : 0;
    }

    private static byte Kind(INamedTypeSymbol symbol) => symbol.TypeKind switch
    {
        TypeKind.Class when symbol.IsRecord => 6,
        TypeKind.Struct when symbol.IsRecord => 7,
        TypeKind.Class => 1,
        TypeKind.Struct => 2,
        TypeKind.Interface => 3,
        TypeKind.Enum => 4,
        TypeKind.Delegate => 5,
        _ => throw new OracleFailure($"Roslyn emitted unsupported declaration kind {symbol.TypeKind}"),
    };

    private readonly record struct BoundDeclaration(INamedTypeSymbol Symbol, uint Start, uint End);
    private readonly record struct Row(byte Kind, uint NameOffset, uint NameLength, uint Start, uint End);
}
