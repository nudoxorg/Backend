using System.Buffers.Binary;
using System.Collections.Generic;
using System.Collections.Immutable;
using System.Security.Cryptography;
using System.Text;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;
using Microsoft.CodeAnalysis.Text;

namespace Nudox.Oracle;

    /// <summary>Writes the canonical version-4 Roslyn authority image.</summary>
internal static class AuthorityImage
{
    private const int HeaderBytes = 256;
    private const uint Absent = uint.MaxValue;
    private static readonly byte[] DigestDomain = "nudox.csharp.authority.image.sha256.v4\0"u8.ToArray();
    private static readonly int[] Widths = [8, 1, 52, 24, 12, 4, 16, 8, 8, 20, 28];

    public static void Write(LoadedCompilation loaded, string sourceBinding, Stream destination)
    {
        var binding = Path.GetFullPath(sourceBinding);
        var tree = loaded.Compilation.SyntaxTrees.SingleOrDefault(t =>
            StringComparer.Ordinal.Equals(Path.GetFullPath(t.FilePath), binding));
        if (tree is null)
            throw new OracleFailure("C# authority source binding is absent from the Roslyn compilation");
        var source = File.ReadAllBytes(binding);
        var offsets = Extractor.BuildByteOffsets(
            tree,
            source.AsSpan().StartsWith(new byte[] { 0xEF, 0xBB, 0xBF }) ? 3 : 0);
        var image = new Builder(loaded.Compilation, tree, offsets).Build(source);
        destination.Write(image);
    }

    private sealed class Builder
    {
        private readonly CSharpCompilation compilation;
        private readonly SyntaxTree tree;
        private readonly int[] offsets;
        private readonly Dictionary<string, uint> atomMap = new(StringComparer.Ordinal);
        private readonly List<byte> atomBytes = [];
        private readonly List<byte[]> declarations = [];
        private readonly List<byte[]> parameters = [];
        private readonly List<byte[]> typeParameters = [];
        private readonly List<byte[]> constraints = [];
        private readonly List<byte[]> types = [];
        private readonly List<byte[]> children = [];
        private readonly List<byte[]> attributes = [];
        private readonly List<byte[]> docs = [];
        private readonly List<byte[]> references = [];
        private readonly List<DeclarationInfo> infos = [];
        private readonly Dictionary<ISymbol, uint> declarationMap = new(SymbolEqualityComparer.Default);
        private readonly Dictionary<SyntaxNode, uint> syntaxMap = new();
        private readonly Dictionary<ITypeSymbol, uint> typeMap = new(SymbolEqualityComparer.Default);
        private readonly HashSet<ITypeSymbol> typeInProgress = new(SymbolEqualityComparer.Default);

        public Builder(CSharpCompilation compilation, SyntaxTree tree, int[] offsets)
        {
            this.compilation = compilation;
            this.tree = tree;
            this.offsets = offsets;
        }

        public byte[] Build(byte[] source)
        {
            var model = compilation.GetSemanticModel(tree);
            foreach (var node in tree.GetRoot().DescendantNodesAndSelf())
            {
                // A field/event declaration is one syntax container but each
                // variable is a distinct Roslyn symbol and semantic entity.
                // Bind the container to the first row for declaration-wide
                // ownership and each declarator to its own row for references
                // in initializers. Selecting Variables[0] here would silently
                // discard every later declarator.
                if (node is BaseFieldDeclarationSyntax field)
                {
                    var first = true;
                    foreach (var variable in field.Declaration.Variables)
                    {
                        var fieldSymbol = model.GetDeclaredSymbol(variable);
                        if (fieldSymbol is null || !IsEmittable(node, fieldSymbol)) continue;
                        var fieldRow = declarations.Count;
                        declarationMap.TryAdd(fieldSymbol, checked((uint)fieldRow));
                        syntaxMap.Add(variable, checked((uint)fieldRow));
                        if (first)
                        {
                            syntaxMap.Add(node, checked((uint)fieldRow));
                            first = false;
                        }
                        infos.Add(new DeclarationInfo(
                            fieldSymbol,
                            node,
                            null,
                            variable.Identifier.Span));
                        declarations.Add(new byte[52]);
                    }
                    continue;
                }
                var symbol = DeclaredSymbol(model, node);
                if (symbol is null || !IsEmittable(node, symbol) || syntaxMap.ContainsKey(node))
                    continue;
                var row = declarations.Count;
                declarationMap.TryAdd(symbol, checked((uint)row));
                syntaxMap.Add(node, checked((uint)row));
                infos.Add(new DeclarationInfo(symbol, node, null, null));
                declarations.Add(new byte[52]);
            }
            // Roslyn exposes record primary-constructor properties as symbols but
            // there is no property declaration node to visit. Preserve the
            // source-site identity by adding one row for each positional
            // parameter, owned by the record declaration.
            foreach (var info in infos.Where(i => i.Symbol is INamedTypeSymbol n && n.IsRecord).ToArray())
            {
                var record = (INamedTypeSymbol)info.Symbol;
                var declaration = info.Node as RecordDeclarationSyntax;
                if (declaration is null) continue;
                foreach (var parameter in declaration.ParameterList?.Parameters ?? [])
                {
                    var property = record.GetMembers(parameter.Identifier.ValueText)
                        .OfType<IPropertySymbol>().FirstOrDefault();
                    if (property is null || declarationMap.ContainsKey(property)) continue;
                    var row = declarations.Count;
                    declarationMap.Add(property, checked((uint)row));
                    infos.Add(new DeclarationInfo(property, declaration,
                        declaration.ParameterList!.Span, parameter.Identifier.Span));
                    declarations.Add(new byte[52]);
                }
            }
            // Named type rows are made before any member rows; all later type uses
            // therefore resolve to the same qualified spelling and stable ordinal.
            foreach (var info in infos.Where(i => i.Symbol is INamedTypeSymbol))
                AddType((ITypeSymbol)info.Symbol);
            for (var i = 0; i < infos.Count; i++) WriteDeclaration(i, infos[i]);
            foreach (var info in infos) WriteAttributesAndDocs(info);
            WriteReferences(model);

            var sections = new List<byte[]> {
                AtomRows(), atomBytes.ToArray(), Rows(declarations), Rows(parameters),
                Rows(typeParameters), Rows(constraints), Rows(types), Rows(children),
                Rows(attributes), Rows(docs), Rows(references)
            };
            return Image(source, sections);
        }

        private static ISymbol? DeclaredSymbol(SemanticModel model, SyntaxNode node) => node switch
        {
            BaseNamespaceDeclarationSyntax n => model.GetDeclaredSymbol(n),
            BaseTypeDeclarationSyntax n => model.GetDeclaredSymbol(n),
            DelegateDeclarationSyntax n => model.GetDeclaredSymbol(n),
            EnumMemberDeclarationSyntax n => model.GetDeclaredSymbol(n),
            PropertyDeclarationSyntax n => model.GetDeclaredSymbol(n),
            IndexerDeclarationSyntax n => model.GetDeclaredSymbol(n),
            EventDeclarationSyntax n => model.GetDeclaredSymbol(n),
            ConstructorDeclarationSyntax n => model.GetDeclaredSymbol(n),
            MethodDeclarationSyntax n => model.GetDeclaredSymbol(n),
            OperatorDeclarationSyntax n => model.GetDeclaredSymbol(n),
            ConversionOperatorDeclarationSyntax n => model.GetDeclaredSymbol(n),
            _ => null
        };

        private static bool IsEmittable(SyntaxNode node, ISymbol symbol) =>
            symbol.Name.Length > 0 && node.SyntaxTree is not null &&
            (symbol.Locations.Any(l => l.IsInSource && l.SourceTree == node.SyntaxTree));

        private void WriteDeclaration(int index, DeclarationInfo info)
        {
            var row = declarations[index];
            var symbol = info.Symbol;
            var span = Span(info.Span);
            var nameSpan = info.NameSpan;
            // A static constructor has no source identifier: Roslyn names it
            // after the containing type in syntax, but its canonical metadata
            // name is `.cctor`, which is the only fact that separates it from
            // the parameterless instance constructor. Every other declaration
            // keeps its exact source spelling.
            var name = symbol is IMethodSymbol { MethodKind: MethodKind.StaticConstructor }
                ? Atom(symbol.Name)
                : Atom(SourceSlice(nameSpan));
            Put(row, 0, Kind(symbol)); row[1] = Flags(symbol, info.Node);
            row[2] = Partial(symbol, tree, info.Node); row[3] = RefKind(symbol);
            Put(row, 4, name); Put(row, 8, symbol is INamedTypeSymbol n ? Atom(Fqn(n)) : Absent);
            Put(row, 12, Owner(symbol));
            Put(row, 16, DeclaredType(symbol));
            Put(row, 20, span.Start); Put(row, 24, U(offsets[nameSpan.Start])); Put(row, 28, U(offsets[nameSpan.End]));
            // UNWIRED (v4 activates it): the exclusive end of the full
            // declaration span, mapped through the same byte-offset table.
            Put(row, 48, span.End);
            var ps = parameters.Count; var gs = typeParameters.Count;
            if (symbol is INamedTypeSymbol delegateType && delegateType.TypeKind == TypeKind.Delegate && delegateType.DelegateInvokeMethod is { } invoke) WriteParameters(invoke.Parameters);
            else if (symbol is IMethodSymbol m) WriteParameters(m.Parameters);
            else if (symbol is IPropertySymbol p) WriteParameters(p.Parameters);
            Put(row, 32, checked((uint)ps)); BinaryPrimitives.WriteUInt16LittleEndian(row.AsSpan(36), checked((ushort)(parameters.Count - ps)));
            if (symbol is INamedTypeSymbol nt) WriteTypeParameters(nt.TypeParameters);
            if (symbol is IMethodSymbol mt) WriteTypeParameters(mt.TypeParameters);
            Put(row, 38, checked((uint)gs)); BinaryPrimitives.WriteUInt16LittleEndian(row.AsSpan(42), checked((ushort)(typeParameters.Count - gs)));
            Put(row, 44, DocRow(symbol));
        }

        private uint DeclaredType(ISymbol symbol) => symbol switch
        {
            INamedTypeSymbol n when n.TypeKind == TypeKind.Delegate => AddType(n.DelegateInvokeMethod?.ReturnType ?? compilation.GetSpecialType(SpecialType.System_Void)),
            IFieldSymbol f => AddType(f.Type),
            IPropertySymbol p => AddType(p.Type),
            IEventSymbol e => AddType(e.Type),
            IMethodSymbol m when m.MethodKind is not (MethodKind.Constructor or MethodKind.StaticConstructor) => AddType(m.ReturnType),
            _ => Absent
        };

        private void WriteParameters(ImmutableArray<IParameterSymbol> list)
        {
            foreach (var parameter in list)
            {
                var row = new byte[24]; Put(row, 0, AddType(parameter.Type)); Put(row, 4, Atom(parameter.Name));
                row[8] = RefKind(parameter); row[9] = (byte)((parameter.IsParams ? 1 : 0) | (HasDefault(parameter) ? 2 : 0));
                Put(row, 12, HasDefault(parameter) ? Atom(Format(parameter.ExplicitDefaultValue)) : Absent);
                var span = parameter.Locations.FirstOrDefault(l => l.IsInSource && l.SourceTree == tree)?.SourceSpan;
                Put(row, 16, span is null ? 0u : U(offsets[span.Value.Start])); Put(row, 20, span is null ? 0u : U(offsets[span.Value.End]));
                parameters.Add(row);
            }
        }

        private void WriteTypeParameters(ImmutableArray<ITypeParameterSymbol> list)
        {
            foreach (var parameter in list)
            {
                var row = new byte[12]; Put(row, 0, Atom(parameter.Name)); Put(row, 4, checked((uint)constraints.Count));
                foreach (var constraint in parameter.ConstraintTypes) { var c = new byte[4]; Put(c, 0, AddType(constraint)); constraints.Add(c); }
                BinaryPrimitives.WriteUInt16LittleEndian(row.AsSpan(8), checked((ushort)parameter.ConstraintTypes.Length));
                row[10] = parameter.Variance switch { VarianceKind.Out => 1, VarianceKind.In => 2, _ => 0 };
                row[11] = (byte)((parameter.HasReferenceTypeConstraint ? 1 : 0) | (parameter.HasValueTypeConstraint ? 2 : 0) |
                    (parameter.HasNotNullConstraint ? 4 : 0) | (parameter.HasUnmanagedTypeConstraint ? 8 : 0) |
                    (parameter.HasConstructorConstraint ? 16 : 0) | (AllowsRefLike(parameter) ? 32 : 0));
                typeParameters.Add(row);
            }
        }

        private uint AddType(ITypeSymbol type)
        {
            if (typeMap.TryGetValue(type, out var known)) return known;
            if (typeInProgress.Contains(type) || typeInProgress.Count >= 64) throw new OracleFailure("C# authority type graph exceeded recursion budget");
            if (types.Count >= 1_000_000) throw new OracleFailure("C# authority type graph exceeds capacity");
            typeInProgress.Add(type); var index = checked((uint)types.Count); typeMap[type] = index;
            var row = new byte[16]; types.Add(row);
            row[0] = type switch
            {
                IArrayTypeSymbol => 2,
                IPointerTypeSymbol => 3,
                IErrorTypeSymbol => 9,
                IDynamicTypeSymbol => 8,
                INamedTypeSymbol n when n.OriginalDefinition.SpecialType == SpecialType.System_Nullable_T => 4,
                INamedTypeSymbol n when n.IsTupleType => 5,
                IFunctionPointerTypeSymbol => 6,
                ITypeParameterSymbol => 7,
                _ => 1
            };
            row[1] = Nullable(type); Put(row, 4, Spelling(type));
            // Resolve descendants before appending this node's rows. Recursive
            // AddType calls may append their own children, so measuring the
            // range around the original loop would accidentally include those
            // descendants in the parent's direct-child count.
            var directChildren = TypeChildren(type)
                .Select(child => (child.Label, Type: AddType(child.Type)))
                .ToArray();
            var start = children.Count;
            foreach (var child in directChildren)
            {
                var cr = new byte[8];
                Put(cr, 0, child.Label is null ? Absent : Atom(child.Label));
                Put(cr, 4, child.Type);
                children.Add(cr);
            }
            Put(row, 8, checked((uint)start)); Put(row, 12, checked((uint)(children.Count - start)));
            if (type is IFunctionPointerTypeSymbol fp && !fp.Signature.ReturnsVoid) row[3] = 1;
            typeInProgress.Remove(type); return index;
        }

        private IEnumerable<(ITypeSymbol Type, string? Label)> TypeChildren(ITypeSymbol type)
        {
            // Error types are leaves by contract: the display-string spelling
            // already carries the unresolved name (including any arity), so
            // their type arguments must not become structural children. Note
            // Roslyn models errors as named types; without this guard an
            // unresolvable generic silently violates the reader's child law.
            if (type is IErrorTypeSymbol) yield break;
            if (type is IArrayTypeSymbol a) { for (var i = 0; i < a.Rank; i++) yield return (a.ElementType, null); yield break; }
            if (type is IPointerTypeSymbol p) { yield return (p.PointedAtType, null); yield break; }
            if (type is INamedTypeSymbol n && n.OriginalDefinition.SpecialType == SpecialType.System_Nullable_T) { yield return (n.TypeArguments[0], null); yield break; }
            if (type is INamedTypeSymbol tuple && tuple.IsTupleType) { foreach (var e in tuple.TupleElements) yield return (e.Type, e.IsExplicitlyNamedTupleElement ? e.Name : null); yield break; }
            if (type is IFunctionPointerTypeSymbol fp) { foreach (var parameter in fp.Signature.Parameters) yield return (parameter.Type, null); if (!fp.Signature.ReturnsVoid) yield return (fp.Signature.ReturnType, null); yield break; }
            if (type is INamedTypeSymbol named) foreach (var arg in named.TypeArguments) yield return (arg, null);
        }

        private uint Spelling(ITypeSymbol type) => type switch
        {
            IErrorTypeSymbol e => Atom(e.ToDisplayString()),
            INamedTypeSymbol n => Atom(Fqn(n)),
            ITypeParameterSymbol parameter => Atom(parameter.Name),
            IArrayTypeSymbol a => Atom(a.ToDisplayString(SymbolDisplayFormat.MinimallyQualifiedFormat)),
            IPointerTypeSymbol pointer => Atom(pointer.ToDisplayString()),
            IFunctionPointerTypeSymbol f => Atom(f.ToDisplayString()),
            _ => Absent
        };

        private void WriteAttributesAndDocs(DeclarationInfo info)
        {
            if (!info.Symbol.Locations.Any(l => l.IsInSource && l.SourceTree == tree)) return;
            var owner = declarationMap[info.Symbol];
            foreach (var attribute in info.Symbol.GetAttributes()) { var row = new byte[8]; Put(row, 0, owner); Put(row, 4, Atom(AttributeText(attribute))); attributes.Add(row); }
            var xml = info.Symbol.GetDocumentationCommentXml(expandIncludes: true);
            if (!string.IsNullOrWhiteSpace(xml))
            {
                var row = new byte[20];
                Put(row, 0, owner); Put(row, 4, Atom(tree.FilePath));
                var comment = DocumentationSpan(info.Node);
                Put(row, 8, U(offsets[comment.Start])); Put(row, 12, U(offsets[comment.End]));
                Put(row, 16, Atom(xml));
                Put(declarations[(int)owner], 44, checked((uint)docs.Count)); docs.Add(row);
            }
        }

        private void WriteReferences(SemanticModel model)
        {
            // File-header usings have no host row; usings nested in declarations do.
            foreach (var node in tree.GetRoot().DescendantNodes().OfType<UsingDirectiveSyntax>())
            {
                var ownerNode = node.Ancestors().FirstOrDefault(n => DeclaredSymbol(model, n) is not null);
                if (ownerNode is null || DeclaredSymbol(model, ownerNode) is not { } owner || !declarationMap.TryGetValue(owner, out var ownerRow)) continue;
                var row = new byte[28]; Put(row, 0, ownerRow); Put(row, 4, Absent); Put(row, 8, Atom(node.Name?.ToString() ?? "")); Put(row, 12, Atom(tree.FilePath)); var s = Span(node); Put(row, 16, s.Start); Put(row, 20, s.End); row[24] = 4; references.Add(row);
            }
            foreach (var node in tree.GetRoot().DescendantNodes().OfType<InvocationExpressionSyntax>()) AddReference(model, node, 1, node.Expression);
            foreach (var node in tree.GetRoot().DescendantNodes().OfType<ObjectCreationExpressionSyntax>()) AddReference(model, node, 2, node.Type);
            foreach (var node in tree.GetRoot().DescendantNodes().OfType<MemberAccessExpressionSyntax>()) AddReference(model, node, 3, node.Name);
            // A bare identifier resolving to a field is a compiler-proved
            // reference even without a member-access receiver: `count`,
            // `count = 5`, `count++`. The read/write split keeps the two
            // classes the helper can prove; a compound assignment or an
            // increment both reads and writes and keeps the write class, the
            // state-changing half. Member-access names are the member-access
            // sweep's rows, and a site Roslyn cannot resolve keeps no row:
            // locals and parameters are not fields, so an unresolved or
            // non-field name has no honest target here.
            foreach (var node in tree.GetRoot().DescendantNodes().OfType<IdentifierNameSyntax>())
            {
                if (node.Parent is MemberAccessExpressionSyntax) continue;
                if (model.GetSymbolInfo(node).Symbol is not IFieldSymbol) continue;
                var write = (node.Parent is AssignmentExpressionSyntax assignment && assignment.Left == node)
                    || node.Parent.IsKind(SyntaxKind.PreIncrementExpression)
                    || node.Parent.IsKind(SyntaxKind.PostIncrementExpression)
                    || node.Parent.IsKind(SyntaxKind.PreDecrementExpression)
                    || node.Parent.IsKind(SyntaxKind.PostDecrementExpression);
                AddReference(model, node, write ? (byte)7 : (byte)6, node);
            }
            foreach (var info in infos)
            {
                var targets = info.Symbol switch
                {
                    IMethodSymbol method => method.ExplicitInterfaceImplementations.Cast<ISymbol>(),
                    IPropertySymbol property => property.ExplicitInterfaceImplementations.Cast<ISymbol>(),
                    IEventSymbol evt => evt.ExplicitInterfaceImplementations.Cast<ISymbol>(),
                    _ => []
                };
                if (!syntaxMap.TryGetValue(info.Node, out var owner)) continue;
                foreach (var target in targets)
                {
                    // An implementation binding whose interface member lives
                    // in another assembly has no local row: the binding is
                    // still real compiler-proved authority, so the row keeps
                    // an absent target and the member name as the foreign
                    // spelling the reader keys on.
                    var targetRow = declarationMap.TryGetValue(target, out var row) ? row : Absent;
                    var span = Span(info.Span);
                    var nameEnd = U(offsets[info.NameSpan.End]);
                    var binding = new byte[28]; Put(binding, 0, owner); Put(binding, 4, targetRow);
                    Put(binding, 8, Atom(target.Name)); Put(binding, 12, Atom(tree.FilePath));
                    Put(binding, 16, span.Start); Put(binding, 20, nameEnd); binding[24] = 5; references.Add(binding);
                }
            }
        }

        private void AddReference(SemanticModel model, SyntaxNode node, byte tag, SyntaxNode spellingNode)
        {
            var ownerNode = node.Ancestors().FirstOrDefault(syntaxMap.ContainsKey);
            if (ownerNode is null || !syntaxMap.TryGetValue(ownerNode, out var ownerRow)) return;
            var target = ResolveTarget(model.GetSymbolInfo(node).Symbol);
            var row = new byte[28]; Put(row, 0, ownerRow); Put(row, 4, target); Put(row, 8, Atom(spellingNode.ToString())); Put(row, 12, Atom(tree.FilePath)); var s = Span(spellingNode); Put(row, 16, s.Start); Put(row, 20, s.End); row[24] = tag; references.Add(row);
        }

        private uint ResolveTarget(ISymbol? symbol)
        {
            if (symbol is null) return Absent;
            if (declarationMap.TryGetValue(symbol, out var row)) return row;
            // An object creation binds to the constructor it invokes, and
            // Roslyn answers the creation's type site with exactly that
            // constructor. A constructor the source never declares (the
            // compiler-generated implicit one) has no declaration row of its
            // own, but the constructed type is declared in this compiled
            // source and is the honest same-file target of the reference.
            // Explicitly declared constructors keep their own rows, and any
            // other unmapped symbol stays unmapped.
            if (symbol is IMethodSymbol { MethodKind: MethodKind.Constructor or MethodKind.StaticConstructor } constructor
                && declarationMap.TryGetValue(constructor.ContainingType, out var typeRow))
                return typeRow;
            return Absent;
        }

        private uint DocRow(ISymbol symbol) => Absent; // populated after docs are written
        private uint Owner(ISymbol symbol) => symbol.ContainingSymbol is { } parent && declarationMap.TryGetValue(parent, out var row) ? row : Absent;
        private byte Kind(ISymbol s) => s switch { INamedTypeSymbol n when n.TypeKind == TypeKind.Class && n.IsRecord => 6, INamedTypeSymbol n when n.TypeKind == TypeKind.Struct && n.IsRecord => 7, INamedTypeSymbol n when n.TypeKind == TypeKind.Class => 1, INamedTypeSymbol n when n.TypeKind == TypeKind.Struct => 2, INamedTypeSymbol n when n.TypeKind == TypeKind.Interface => 3, INamedTypeSymbol n when n.TypeKind == TypeKind.Enum => 4, INamedTypeSymbol n when n.TypeKind == TypeKind.Delegate => 5, INamespaceSymbol => 8, IFieldSymbol f when f.ContainingType?.TypeKind == TypeKind.Enum => 10, IFieldSymbol => 9, IPropertySymbol p when p.IsIndexer => 12, IPropertySymbol => 11, IEventSymbol => 13, IMethodSymbol m when m.MethodKind == MethodKind.StaticConstructor => 18, IMethodSymbol m when m.MethodKind == MethodKind.Constructor => 14, IMethodSymbol m when m.MethodKind == MethodKind.UserDefinedOperator => 16, IMethodSymbol m when m.MethodKind == MethodKind.Conversion => 17, IMethodSymbol => 15, _ => throw new OracleFailure("Roslyn emitted unsupported declaration kind") };
        private static byte RefKind(ISymbol s) => s is IMethodSymbol method ? RefKind(method.RefKind) : s is IPropertySymbol property && property.ReturnsByRefReadonly ? (byte)4 : s is IPropertySymbol property2 && property2.ReturnsByRef ? (byte)2 : (byte)0;
        private static byte RefKind(RefKind k) => k switch { Microsoft.CodeAnalysis.RefKind.In => 1, Microsoft.CodeAnalysis.RefKind.Ref => 2, Microsoft.CodeAnalysis.RefKind.Out => 3, Microsoft.CodeAnalysis.RefKind.RefReadOnlyParameter => 4, _ => 0 };
        private static byte Flags(ISymbol s, SyntaxNode n) => (byte)((s is IMethodSymbol method && method.IsExtensionMethod ? 1 : 0) | (s is IMethodSymbol asyncMethod && asyncMethod.IsAsync ? 2 : 0) | (s is IMethodSymbol iterator && HasYield(n) ? 4 : 0) | (s is IFieldSymbol field && field.IsConst ? 8 : 0) | (s switch { IMethodSymbol m => m.ExplicitInterfaceImplementations.Length > 0, IPropertySymbol p => p.ExplicitInterfaceImplementations.Length > 0, IEventSymbol e => e.ExplicitInterfaceImplementations.Length > 0, _ => false } ? 16 : 0));
        private static byte Partial(ISymbol symbol, SyntaxTree tree, SyntaxNode node)
        {
            if (symbol is INamedTypeSymbol named && named.DeclaringSyntaxReferences.Length > 1)
            {
                // Every independently compiled source fragment retains its
                // local partial type. When the same tree has several parts,
                // only its earliest source part is the definition row; later
                // parts are implementation rows and lower under that owner.
                var firstLocal = named.DeclaringSyntaxReferences
                    .Where(reference => reference.SyntaxTree == tree)
                    .OrderBy(reference => reference.Span.Start)
                    .FirstOrDefault();
                return firstLocal is null || firstLocal.Span.Equals(node.Span) ? (byte)1 : (byte)2;
            }
            if (symbol is not IMethodSymbol method ||
                (method.PartialDefinitionPart is null && method.PartialImplementationPart is null))
                return 0;

            var definition = method.PartialDefinitionPart ??
                (method.PartialImplementationPart is not null ? method : null);
            var implementation = method.PartialImplementationPart ??
                (method.PartialDefinitionPart is not null ? method : null);
            if (Matches(definition, node)) return 1;
            if (Matches(implementation, node)) return 2;
            return 0;
        }
        private static byte Nullable(ITypeSymbol t) => t.NullableAnnotation switch { NullableAnnotation.Annotated => 1, NullableAnnotation.NotAnnotated when t.IsReferenceType || t.TypeKind == TypeKind.TypeParameter => 2, _ => 0 };
        private static bool HasDefault(IParameterSymbol p) { try { return p.HasExplicitDefaultValue; } catch (InvalidOperationException) { return false; } }
        private static bool AllowsRefLike(ITypeParameterSymbol p) => typeof(ITypeParameterSymbol).GetProperty("AllowsRefLikeType")?.GetValue(p) is true;
        private static bool HasYield(SyntaxNode n) => n.DescendantNodes(x => x == n || x is not (LocalFunctionStatementSyntax or AnonymousFunctionExpressionSyntax)).OfType<YieldStatementSyntax>().Any();
        private static string Fqn(INamedTypeSymbol n) => n.ToDisplayString(SymbolDisplayFormat.FullyQualifiedFormat).Replace("global::", "", StringComparison.Ordinal);
        private static string AttributeText(AttributeData a) => a.ToString() ?? "";
        private static string Format(object? value) => value is null ? "null" : SymbolDisplay.FormatPrimitive(value, true, false) ?? "null";
        private uint Atom(string value) => Atom(Encoding.UTF8.GetBytes(value));
        private uint Atom(byte[] value) { if (value.Length == 0) throw new OracleFailure("C# authority atom cannot be empty"); var key = Convert.ToBase64String(value); if (atomMap.TryGetValue(key, out var i)) return i; i = checked((uint)atomMap.Count); atomMap.Add(key, i); atomBytes.AddRange(value); return i; }
        private (uint Start, uint End) Span(TextSpan span) => (U(offsets[span.Start]), U(offsets[span.End]));
        private (uint Start, uint End) Span(SyntaxNode n) => Span(n.Span);
        private TextSpan NameSpan(SyntaxNode n) => n switch { BaseNamespaceDeclarationSyntax x => x.Name.Span, BaseTypeDeclarationSyntax x => x.Identifier.Span, DelegateDeclarationSyntax x => x.Identifier.Span, EventFieldDeclarationSyntax x => x.Declaration.Variables[0].Identifier.Span, BaseFieldDeclarationSyntax x => x.Declaration.Variables[0].Identifier.Span, EnumMemberDeclarationSyntax x => x.Identifier.Span, PropertyDeclarationSyntax x => x.Identifier.Span, IndexerDeclarationSyntax x => x.ThisKeyword.Span, EventDeclarationSyntax x => x.Identifier.Span, ConstructorDeclarationSyntax x => x.Identifier.Span, MethodDeclarationSyntax x => x.Identifier.Span, OperatorDeclarationSyntax x => x.OperatorToken.Span, ConversionOperatorDeclarationSyntax x => x.Type.Span, _ => n.Span };
        private byte[] SourceSlice(TextSpan span) => Encoding.UTF8.GetBytes(tree.GetText().ToString(span));
        private static TextSpan DocumentationSpan(SyntaxNode n) => n.GetLeadingTrivia().Where(t => t.IsKind(SyntaxKind.SingleLineDocumentationCommentTrivia) || t.IsKind(SyntaxKind.MultiLineDocumentationCommentTrivia)).Select(t => t.FullSpan).LastOrDefault();
        private static bool Matches(ISymbol? symbol, SyntaxNode node) => symbol?.DeclaringSyntaxReferences.Any(reference => reference.Span == node.Span && reference.SyntaxTree == node.SyntaxTree) == true;
        private static uint U(int value) => checked((uint)value);
        private static void Put(byte[] row, int offset, uint value) => BinaryPrimitives.WriteUInt32LittleEndian(row.AsSpan(offset, 4), value);
        private static byte[] Rows(List<byte[]> rows) { var output = new byte[rows.Sum(r => r.Length)]; var at = 0; foreach (var row in rows) { row.CopyTo(output, at); at += row.Length; } return output; }
        private byte[] AtomRows() { var rows = new byte[atomMap.Count * 8]; var ordered = atomMap.OrderBy(p => p.Value); var at = 0; var offset = 0u; foreach (var pair in ordered) { var bytes = Convert.FromBase64String(pair.Key); Put(rows, at, offset); Put(rows, at + 4, checked((uint)bytes.Length)); offset += checked((uint)bytes.Length); at += 8; } return rows; }
        private static byte[] Image(byte[] source, List<byte[]> sections)
        {
            var body = sections.Sum(s => s.Length); var image = new byte[HeaderBytes + body]; "NCAI"u8.CopyTo(image); BinaryPrimitives.WriteUInt16LittleEndian(image.AsSpan(4), 4); BinaryPrimitives.WriteUInt16LittleEndian(image.AsSpan(6), HeaderBytes); BinaryPrimitives.WriteUInt32LittleEndian(image.AsSpan(8), checked((uint)body)); SHA256.HashData(source).CopyTo(image, 12); BinaryPrimitives.WriteUInt16LittleEndian(image.AsSpan(44), 11);
            var offset = HeaderBytes; for (var i = 0; i < 11; i++) { var at = 48 + i * 16; BinaryPrimitives.WriteUInt16LittleEndian(image.AsSpan(at), checked((ushort)(i + 1))); BinaryPrimitives.WriteUInt16LittleEndian(image.AsSpan(at + 2), checked((ushort)Widths[i])); BinaryPrimitives.WriteUInt32LittleEndian(image.AsSpan(at + 4), checked((uint)(sections[i].Length / Widths[i]))); BinaryPrimitives.WriteUInt32LittleEndian(image.AsSpan(at + 8), checked((uint)offset)); BinaryPrimitives.WriteUInt32LittleEndian(image.AsSpan(at + 12), checked((uint)sections[i].Length)); sections[i].CopyTo(image, offset); offset += sections[i].Length; }
            using var hash = IncrementalHash.CreateHash(HashAlgorithmName.SHA256); hash.AppendData(DigestDomain); hash.AppendData(image, 0, 224); hash.AppendData(image, HeaderBytes, body); hash.GetHashAndReset().CopyTo(image, 224); return image;
        }
    }

    private sealed record DeclarationInfo(ISymbol Symbol, SyntaxNode Node, TextSpan? DeclarationSpan, TextSpan? ExplicitNameSpan)
    {
        public TextSpan Span => DeclarationSpan ?? Node.Span;
        public TextSpan NameSpan => ExplicitNameSpan ?? Node switch
        {
            BaseNamespaceDeclarationSyntax x => x.Name.Span,
            BaseTypeDeclarationSyntax x => x.Identifier.Span,
            DelegateDeclarationSyntax x => x.Identifier.Span,
            EventFieldDeclarationSyntax x => x.Declaration.Variables[0].Identifier.Span,
            BaseFieldDeclarationSyntax x => x.Declaration.Variables[0].Identifier.Span,
            EnumMemberDeclarationSyntax x => x.Identifier.Span,
            PropertyDeclarationSyntax x => x.Identifier.Span,
            IndexerDeclarationSyntax x => x.ThisKeyword.Span,
            EventDeclarationSyntax x => x.Identifier.Span,
            ConstructorDeclarationSyntax x => x.Identifier.Span,
            MethodDeclarationSyntax x => x.Identifier.Span,
            OperatorDeclarationSyntax x => x.OperatorToken.Span,
            ConversionOperatorDeclarationSyntax x => x.Type.Span,
            _ => Node.Span
        };
    }
}
