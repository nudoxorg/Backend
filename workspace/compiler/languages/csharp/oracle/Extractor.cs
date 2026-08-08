using System.Reflection;
using System.Text.Json;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;

namespace Nudox.Oracle;

/// <summary>
/// Walks a bound compilation and writes the schema document.
/// </summary>
/// <remarks>
/// <para>
/// The document is a <i>flat</i> list of type declarations joined by
/// <c>enclosing</c> parent pointers, because the Rust lowering consumes it in a
/// single pass with forward references and builds no intermediate tree.
/// </para>
/// <para>
/// <b>Only the source assembly is walked.</b> <c>Compilation.GlobalNamespace</c>
/// merges every referenced assembly into one namespace tree, so walking it emits
/// the entire base class library — an early version of this extractor produced a
/// 17.8 MB document for a forty-line input that way.
/// <c>Compilation.Assembly.GlobalNamespace</c> is the source assembly alone.
/// </para>
/// </remarks>
internal sealed class Extractor(LoadedCompilation loaded, OracleOptions options)
{
    private readonly TypeSigWriter _types = new();

    /// <summary>
    /// Method kinds that are an accessor for some other member.
    /// </summary>
    /// <remarks>
    /// These are real symbols with real doc-ids, but they duplicate the
    /// property/event/delegate they belong to. Emitting them would double-count
    /// the API and, worse, give a property two IR entries. Note that filtering
    /// on <c>IsImplicitlyDeclared</c> alone is not enough: an accessor declared
    /// in an interface is explicit in source and would survive that check.
    /// </remarks>
    private static readonly HashSet<MethodKind> AccessorKinds =
    [
        MethodKind.PropertyGet,
        MethodKind.PropertySet,
        MethodKind.EventAdd,
        MethodKind.EventRemove,
        MethodKind.EventRaise,
        MethodKind.DelegateInvoke,
        MethodKind.LocalFunction,
        MethodKind.AnonymousFunction,
        MethodKind.BuiltinOperator,
    ];

    public void WriteExtraction(Utf8JsonWriter json, int format)
    {
        var declarations = CollectTypes();

        json.WriteStartObject();

        json.WriteNumber("format", format);
        json.WriteString("dotnetVersion", Environment.Version.ToString());
        json.WriteString("roslyn", RoslynVersion());
        json.WriteString("mode", options.Mode == OracleMode.Source ? "source" : "metadata");

        WriteAssembly(json);
        WriteNamespaces(json, declarations);

        json.WriteStartArray("types");
        foreach (var declaration in declarations)
        {
            WriteTypeDecl(json, declaration);
        }

        json.WriteEndArray();

        // Written last because `errorTypeCount` is only known once every type
        // signature has been visited. JSON object member order is not
        // significant to the deserializer, so this costs nothing.
        json.WriteStartObject("diagnostics");
        json.WriteNumber("errorTypeCount", _types.ErrorTypeCount);
        json.WriteNumber("errorCount", loaded.ErrorCount);
        json.WriteEndObject();

        json.WriteEndObject();
    }

    // ── Assembly and namespaces ──────────────────────────────────────────────

    private void WriteAssembly(Utf8JsonWriter json)
    {
        var assembly = loaded.Compilation.Assembly;

        json.WriteStartObject("assembly");
        json.WriteString("name", loaded.AssemblyName);

        var version = assembly.Identity.Version;
        json.WriteString("version", version.ToString());
        json.WriteString("tfm", loaded.TargetFramework);

        // Type forwarding is a metadata-only construct: a source assembly has no
        // forwarders until it is emitted. Emitting an empty list states that
        // plainly rather than implying none exist in the shipped package.
        json.WriteStartArray("forwardedTypes");
        json.WriteEndArray();

        json.WriteStartArray("ivt");
        foreach (var attribute in assembly.GetAttributes())
        {
            if (attribute.AttributeClass?.Name != "InternalsVisibleToAttribute")
            {
                continue;
            }

            if (attribute.ConstructorArguments.Length > 0
                && attribute.ConstructorArguments[0].Value is string target)
            {
                json.WriteStringValue(target);
            }
        }

        json.WriteEndArray();
        json.WriteEndObject();
    }

    private static void WriteNamespaces(Utf8JsonWriter json, IReadOnlyList<INamedTypeSymbol> types)
    {
        var names = new SortedSet<string>(StringComparer.Ordinal);
        foreach (var type in types)
        {
            names.Add(TypeSigWriter.NamespaceName(type.ContainingNamespace));
        }

        json.WriteStartArray("namespaces");
        foreach (var name in names)
        {
            json.WriteStartObject();
            json.WriteString("name", name);
            // C# has no syntax for documenting a namespace; the field exists for
            // the rare package that ships a NamespaceDoc convention, which this
            // source-mode extractor does not attempt to interpret.
            json.WriteNull("doc");
            json.WriteEndObject();
        }

        json.WriteEndArray();
    }

    // ── Type collection ──────────────────────────────────────────────────────

    /// <summary>All emitted types, ordered by doc-id for a reproducible document.</summary>
    private List<INamedTypeSymbol> CollectTypes()
    {
        var collected = new List<INamedTypeSymbol>();
        Visit(loaded.Compilation.Assembly.GlobalNamespace);
        collected.Sort(static (a, b) => string.CompareOrdinal(DocId(a), DocId(b)));
        return collected;

        void Visit(INamespaceOrTypeSymbol container)
        {
            foreach (var member in container.GetMembers())
            {
                switch (member)
                {
                    case INamespaceSymbol ns:
                        Visit(ns);
                        break;

                    case INamedTypeSymbol type:
                        if (IncludeType(type))
                        {
                            collected.Add(type);
                        }

                        Visit(type);
                        break;
                }
            }
        }
    }

    /// <summary>
    /// Whether a type is emitted.
    /// </summary>
    /// <remarks>
    /// <para>
    /// <b>The containment invariant.</b> A nested type is emitted only when its
    /// container is. `TypeDecl.enclosing` is the one field the Rust lowering
    /// dereferences unconditionally — `parent_ref` calls `Lowering::refer` on it
    /// without checking — and `Lowering::finish` rejects any id that was
    /// referred to but never declared. So an `enclosing` pointing at a filtered
    /// type does not degrade gracefully; it fails the whole package.
    /// </para>
    /// <para>
    /// Enforcing it here rather than teaching the lowering to skip dangling
    /// parents is deliberate: the document is this program's output, and a
    /// document whose parent pointers can dangle is the defect. Filtering is
    /// currently reachable through <c>--public-only</c> and through the
    /// compiler-generated checks below.
    /// </para>
    /// </remarks>
    private bool IncludeType(INamedTypeSymbol type)
    {
        if (type.IsImplicitlyDeclared)
        {
            return false;
        }

        // Compiler-generated names are not valid C# identifiers and never form
        // part of an API a reader can call.
        if (type.MetadataName.StartsWith('<'))
        {
            return false;
        }

        if (!options.IncludeNonPublic && !IsApiSurface(type))
        {
            return false;
        }

        return type.ContainingType is not { } containing || IncludeType(containing);
    }

    /// <summary>Whether a symbol is reachable from outside its assembly.</summary>
    private static bool IsApiSurface(ISymbol symbol)
    {
        for (var current = symbol; current is not null; current = current.ContainingType)
        {
            switch (current.DeclaredAccessibility)
            {
                case Accessibility.Public:
                case Accessibility.Protected:
                case Accessibility.ProtectedOrInternal:
                    continue;
                default:
                    return false;
            }
        }

        return true;
    }

    // ── Type declarations ────────────────────────────────────────────────────

    private void WriteTypeDecl(Utf8JsonWriter json, INamedTypeSymbol type)
    {
        var docs = DocComments.For(type);

        json.WriteStartObject();
        json.WriteString("docId", DocId(type));
        json.WriteString("qualifiedName", TypeSigWriter.MetadataFqn(type));
        json.WriteString("simpleName", SimpleName(type));
        json.WriteString("kind", TypeDeclKind(type));
        json.WriteString("namespace", TypeSigWriter.NamespaceName(type.ContainingNamespace));

        if (type.ContainingType is { } enclosing)
        {
            json.WriteString("enclosing", DocId(enclosing));
        }
        else
        {
            json.WriteNull("enclosing");
        }

        json.WriteStartArray("modifiers");
        foreach (var modifier in Modifiers(type))
        {
            json.WriteStringValue(modifier);
        }

        json.WriteEndArray();

        WriteTypeParams(json, type.TypeParameters);

        json.WritePropertyName("baseType");
        if (type.BaseType is { } baseType)
        {
            _types.Write(json, baseType);
        }
        else
        {
            json.WriteNullValue();
        }

        json.WriteStartArray("interfaces");
        foreach (var iface in type.Interfaces)
        {
            _types.Write(json, iface);
        }

        json.WriteEndArray();

        json.WritePropertyName("enumUnderlying");
        if (type.EnumUnderlyingType is { } underlying)
        {
            _types.Write(json, underlying);
        }
        else
        {
            json.WriteNullValue();
        }

        json.WritePropertyName("delegateSig");
        if (type.DelegateInvokeMethod is { } invoke)
        {
            json.WriteStartObject();
            WriteParameters(json, "params", invoke.Parameters);
            json.WritePropertyName("return");
            _types.Write(json, invoke.ReturnType);
            json.WriteEndObject();
        }
        else
        {
            json.WriteNullValue();
        }

        WriteAttributes(json, type.GetAttributes());
        WriteDeprecated(json, type.GetAttributes());
        json.WriteBoolean("hidden", IsHidden(type.GetAttributes()));
        json.WriteBoolean("forwarded", false);
        WriteDocs(json, docs);

        // A C# 14 `extension(T receiver)` block is compiled to a container type
        // whose receiver is not otherwise recoverable from its members.
        json.WritePropertyName("extensionReceiver");
        if (ExtensionReceiver(type) is { } receiver)
        {
            _types.Write(json, receiver);
        }
        else
        {
            json.WriteNullValue();
        }

        WriteMembers(json, type);

        json.WriteEndObject();
    }

    /// <summary>
    /// The receiver of a C# 14 extension block, when the running Roslyn exposes it.
    /// </summary>
    /// <remarks>
    /// <c>INamedTypeSymbol.ExtensionParameter</c> arrived with C# 14. Binding to
    /// it reflectively keeps this file compiling against a Roslyn that predates
    /// it, and a missing property yields <c>null</c> — the same answer as "this
    /// type is not an extension block" — rather than a load failure at startup.
    /// </remarks>
    private static ITypeSymbol? ExtensionReceiver(INamedTypeSymbol type)
    {
        var property = typeof(INamedTypeSymbol).GetProperty(
            "ExtensionParameter", BindingFlags.Public | BindingFlags.Instance);

        if (property?.GetValue(type) is IParameterSymbol parameter)
        {
            return parameter.Type;
        }

        return null;
    }

    private static string TypeDeclKind(INamedTypeSymbol type) => type.TypeKind switch
    {
        TypeKind.Interface => "INTERFACE",
        TypeKind.Enum => "ENUM",
        TypeKind.Delegate => "DELEGATE",
        TypeKind.Struct => type.IsRecord ? "RECORD_STRUCT" : "STRUCT",
        _ => type.IsRecord ? "RECORD" : "CLASS",
    };

    /// <summary>
    /// The declaration's modifiers, access modifier first.
    /// </summary>
    /// <remarks>
    /// The Rust side reads the first access token to decide visibility and
    /// renders the rest into a `Declared:` note, so order matters. Implicit
    /// modifiers are deliberately not forwarded: Roslyn reports a static class
    /// as abstract *and* sealed, and an interface as abstract, none of which the
    /// author wrote — rendering them would produce `static sealed abstract class`.
    /// </remarks>
    private static List<string> Modifiers(INamedTypeSymbol type)
    {
        var modifiers = new List<string> { AccessibilityToken(type.DeclaredAccessibility) };
        var written = SyntaxModifiers(type);

        if (type.IsStatic)
        {
            modifiers.Add("static");
        }
        else if (type.TypeKind == TypeKind.Class)
        {
            if (type.IsAbstract)
            {
                modifiers.Add("abstract");
            }

            if (type.IsSealed)
            {
                modifiers.Add("sealed");
            }
        }

        if (type.TypeKind == TypeKind.Struct)
        {
            if (type.IsReadOnly)
            {
                modifiers.Add("readonly");
            }

            if (type.IsRefLikeType)
            {
                modifiers.Add("ref");
            }
        }

        foreach (var keyword in new[] { "partial", "unsafe", "new" })
        {
            if (written.Contains(keyword))
            {
                modifiers.Add(keyword);
            }
        }

        return modifiers;
    }

    /// <summary>The modifier keywords actually written in source.</summary>
    private static HashSet<string> SyntaxModifiers(INamedTypeSymbol type)
    {
        var written = new HashSet<string>(StringComparer.Ordinal);

        foreach (var reference in type.DeclaringSyntaxReferences)
        {
            var tokens = reference.GetSyntax() switch
            {
                BaseTypeDeclarationSyntax declaration => declaration.Modifiers,
                DelegateDeclarationSyntax declaration => declaration.Modifiers,
                _ => default,
            };

            foreach (var token in tokens)
            {
                written.Add(token.ValueText);
            }
        }

        return written;
    }

    // ── Members ──────────────────────────────────────────────────────────────

    private void WriteMembers(Utf8JsonWriter json, INamedTypeSymbol type)
    {
        var fields = new List<IFieldSymbol>();
        var properties = new List<IPropertySymbol>();
        var indexers = new List<IPropertySymbol>();
        var events = new List<IEventSymbol>();
        var constructors = new List<IMethodSymbol>();
        var methods = new List<IMethodSymbol>();
        var operators = new List<IMethodSymbol>();
        var conversions = new List<IMethodSymbol>();
        var nested = new List<INamedTypeSymbol>();

        foreach (var member in type.GetMembers())
        {
            if (member is INamedTypeSymbol nestedType)
            {
                if (IncludeType(nestedType))
                {
                    nested.Add(nestedType);
                }

                continue;
            }

            if (!IncludeMember(member))
            {
                continue;
            }

            switch (member)
            {
                case IFieldSymbol field:
                    fields.Add(field);
                    break;
                case IPropertySymbol property:
                    (property.IsIndexer ? indexers : properties).Add(property);
                    break;
                case IEventSymbol evt:
                    events.Add(evt);
                    break;
                case IMethodSymbol method:
                    switch (method.MethodKind)
                    {
                        case MethodKind.Constructor:
                        case MethodKind.StaticConstructor:
                            constructors.Add(method);
                            break;
                        case MethodKind.UserDefinedOperator:
                            operators.Add(method);
                            break;
                        case MethodKind.Conversion:
                            conversions.Add(method);
                            break;
                        default:
                            methods.Add(method);
                            break;
                    }

                    break;
            }
        }

        json.WriteStartObject("members");

        json.WriteStartArray("fields");
        foreach (var field in fields)
        {
            WriteField(json, field);
        }

        json.WriteEndArray();

        json.WriteStartArray("properties");
        foreach (var property in properties)
        {
            WriteProperty(json, property);
        }

        json.WriteEndArray();

        json.WriteStartArray("events");
        foreach (var evt in events)
        {
            WriteEvent(json, evt);
        }

        json.WriteEndArray();

        WriteMethodArray(json, "constructors", constructors);
        WriteMethodArray(json, "methods", methods);
        WriteMethodArray(json, "operators", operators);
        WriteMethodArray(json, "conversions", conversions);

        json.WriteStartArray("indexers");
        foreach (var indexer in indexers)
        {
            WriteProperty(json, indexer);
        }

        json.WriteEndArray();

        json.WriteStartArray("nested");
        foreach (var nestedType in nested)
        {
            json.WriteStringValue(DocId(nestedType));
        }

        json.WriteEndArray();
        json.WriteEndObject();
    }

    private void WriteMethodArray(
        Utf8JsonWriter json, string property, IReadOnlyList<IMethodSymbol> methods)
    {
        json.WriteStartArray(property);
        foreach (var method in methods)
        {
            WriteMethod(json, method);
        }

        json.WriteEndArray();
    }

    private bool IncludeMember(ISymbol member)
    {
        // Records synthesise Equals/GetHashCode/ToString/op_Equality/<Clone>$/
        // Deconstruct/EqualityContract and a copy constructor; property backing
        // fields and the default constructor are synthesised for every type.
        // None of it was written by the author, and all of it is marked implicit.
        if (member.IsImplicitlyDeclared)
        {
            return false;
        }

        if (member is IMethodSymbol method && AccessorKinds.Contains(method.MethodKind))
        {
            return false;
        }

        if (member.MetadataName.StartsWith('<'))
        {
            return false;
        }

        return options.IncludeNonPublic || IsApiSurface(member);
    }

    private void WriteField(Utf8JsonWriter json, IFieldSymbol field)
    {
        var docs = DocComments.For(field);
        var attributes = field.GetAttributes();

        json.WriteStartObject();
        json.WriteString("name", field.Name);
        json.WriteString("docId", DocId(field));
        json.WritePropertyName("type");
        _types.Write(json, field.Type);
        json.WriteString("accessibility", AccessibilityToken(field.DeclaredAccessibility));
        json.WriteBoolean("isConst", field.IsConst);

        json.WritePropertyName("constant");
        if (field.HasConstantValue)
        {
            json.WriteStringValue(FormatConstant(field.ConstantValue));
        }
        else
        {
            json.WriteNullValue();
        }

        json.WriteBoolean("isReadonly", field.IsReadOnly);
        json.WriteBoolean("isVolatile", field.IsVolatile);
        json.WriteBoolean("isRequired", field.IsRequired);
        json.WriteBoolean("isStatic", field.IsStatic);

        WriteAttributes(json, attributes);
        WriteDeprecated(json, attributes);
        json.WriteBoolean("hidden", IsHidden(attributes));
        WriteDocs(json, docs);

        json.WriteEndObject();
    }

    private void WriteProperty(Utf8JsonWriter json, IPropertySymbol property)
    {
        var docs = DocComments.For(property);
        var attributes = property.GetAttributes();

        json.WriteStartObject();
        json.WriteString("name", property.Name);
        json.WriteString("docId", DocId(property));
        json.WritePropertyName("type");
        _types.Write(json, property.Type);
        json.WriteString("accessibility", AccessibilityToken(property.DeclaredAccessibility));

        WriteAccessorAccessibility(json, "getAccessibility", property.GetMethod);
        WriteAccessorAccessibility(json, "setAccessibility", property.SetMethod);

        json.WriteString(
            "setKind",
            property.SetMethod is null ? "none" : property.SetMethod.IsInitOnly ? "init" : "set");

        json.WriteBoolean("isRequired", property.IsRequired);
        json.WriteBoolean("isStatic", property.IsStatic);
        json.WriteBoolean("isIndexer", property.IsIndexer);

        WriteParameters(json, "parameters", property.Parameters);

        json.WriteBoolean("returnsByRef", property.ReturnsByRef);
        json.WriteBoolean("returnsByRefReadonly", property.ReturnsByRefReadonly);

        WriteAttributes(json, attributes);
        WriteDeprecated(json, attributes);
        json.WriteBoolean("hidden", IsHidden(attributes));
        WriteDocs(json, docs);

        json.WriteEndObject();
    }

    private void WriteEvent(Utf8JsonWriter json, IEventSymbol evt)
    {
        var docs = DocComments.For(evt);
        var attributes = evt.GetAttributes();

        json.WriteStartObject();
        json.WriteString("name", evt.Name);
        json.WriteString("docId", DocId(evt));
        json.WritePropertyName("type");
        _types.Write(json, evt.Type);
        json.WriteString("accessibility", AccessibilityToken(evt.DeclaredAccessibility));

        WriteAccessorAccessibility(json, "addAccessibility", evt.AddMethod);
        WriteAccessorAccessibility(json, "removeAccessibility", evt.RemoveMethod);

        json.WriteBoolean("isStatic", evt.IsStatic);

        WriteAttributes(json, attributes);
        WriteDeprecated(json, attributes);
        json.WriteBoolean("hidden", IsHidden(attributes));
        WriteDocs(json, docs);

        json.WriteEndObject();
    }

    private void WriteMethod(Utf8JsonWriter json, IMethodSymbol method)
    {
        var docs = DocComments.For(method);
        var attributes = method.GetAttributes();
        var explicitImpl = method.ExplicitInterfaceImplementations.FirstOrDefault();

        json.WriteStartObject();
        json.WriteString("name", MethodName(method, explicitImpl));
        json.WriteString("docId", DocId(method));
        json.WriteString("methodKind", method.MethodKind.ToString());
        json.WriteString("accessibility", AccessibilityToken(method.DeclaredAccessibility));

        json.WriteBoolean("isStatic", method.IsStatic);
        json.WriteBoolean("isAbstract", method.IsAbstract);
        json.WriteBoolean("isVirtual", method.IsVirtual);
        json.WriteBoolean("isOverride", method.IsOverride);
        json.WriteBoolean("isSealed", method.IsSealed);
        json.WriteBoolean("isExtern", method.IsExtern);
        json.WriteBoolean("isAsync", method.IsAsync);
        json.WriteBoolean("isIterator", IsIterator(method));
        json.WriteBoolean("isExtensionMethod", method.IsExtensionMethod);
        json.WriteBoolean("isReadonly", method.IsReadOnly);

        WriteTypeParams(json, method.TypeParameters);
        WriteParameters(json, "parameters", method.Parameters);

        json.WritePropertyName("returnType");
        if (method.MethodKind is MethodKind.Constructor or MethodKind.StaticConstructor)
        {
            // A constructor has no return type at all — distinct from `void`,
            // which the schema carries as a named `System.Void` node.
            json.WriteNullValue();
        }
        else
        {
            _types.Write(json, method.ReturnType);
        }

        json.WriteBoolean("returnsByRef", method.ReturnsByRef);
        json.WriteBoolean("returnsByRefReadonly", method.ReturnsByRefReadonly);

        // The *interface's* qualified name, not the implemented member's: the
        // Rust side renders `simple_name(explicitInterface) + "." + name`, which
        // yields the documented `IFoo.Bar` form only with the type here.
        json.WritePropertyName("explicitInterface");
        if (explicitImpl?.ContainingType is { } iface)
        {
            json.WriteStringValue(TypeSigWriter.MetadataFqn(iface));
        }
        else
        {
            json.WriteNullValue();
        }

        json.WriteString("operatorKind", OperatorKind(method));

        WriteAttributes(json, attributes);
        WriteDeprecated(json, attributes);
        json.WriteBoolean("hidden", IsHidden(attributes));
        WriteDocs(json, docs);

        json.WriteEndObject();
    }

    /// <summary>
    /// The member's own name, with any explicit-interface qualification removed.
    /// </summary>
    /// <remarks>
    /// Roslyn names an explicit implementation
    /// <c>System.IDisposable.Dispose</c>. Left whole, the Rust side would render
    /// it as <c>IDisposable.System.IDisposable.Dispose</c>, since it prepends
    /// the interface itself.
    /// </remarks>
    private static string MethodName(IMethodSymbol method, ISymbol? explicitImpl)
    {
        if (explicitImpl is null)
        {
            return method.MetadataName;
        }

        var name = method.MetadataName;
        var lastDot = name.LastIndexOf('.');
        return lastDot >= 0 && lastDot + 1 < name.Length ? name[(lastDot + 1)..] : name;
    }

    private static string OperatorKind(IMethodSymbol method)
    {
        var name = method.MetadataName;

        // C# 11 checked operators are spelled `op_CheckedAddition` and are a
        // separate overload from the unchecked one.
        if (name.StartsWith("op_Checked", StringComparison.Ordinal))
        {
            return "checked";
        }

        return name switch
        {
            "op_Implicit" => "implicit",
            "op_Explicit" => "explicit",
            _ => "none",
        };
    }

    /// <summary>Whether the method body contains a <c>yield</c>.</summary>
    /// <remarks>
    /// Roslyn exposes no <c>IsIterator</c> on the public symbol API, so the
    /// syntax is the only source of truth. Nested lambdas and local functions are
    /// not descended into: a <c>yield</c> inside one makes <i>that</i> function an
    /// iterator, not its enclosing method.
    /// </remarks>
    private static bool IsIterator(IMethodSymbol method)
    {
        foreach (var reference in method.DeclaringSyntaxReferences)
        {
            var root = reference.GetSyntax();
            var descendants = root.DescendantNodes(node =>
                node == root
                || node is not (LocalFunctionStatementSyntax or AnonymousFunctionExpressionSyntax));

            foreach (var node in descendants)
            {
                if (node is YieldStatementSyntax)
                {
                    return true;
                }
            }
        }

        return false;
    }

    // ── Shared member fragments ──────────────────────────────────────────────

    private static void WriteAccessorAccessibility(
        Utf8JsonWriter json, string property, IMethodSymbol? accessor)
    {
        if (accessor is null)
        {
            json.WriteNull(property);
        }
        else
        {
            json.WriteString(property, AccessibilityToken(accessor.DeclaredAccessibility));
        }
    }

    private void WriteTypeParams(
        Utf8JsonWriter json, IReadOnlyList<ITypeParameterSymbol> typeParameters)
    {
        json.WriteStartArray("typeParams");
        foreach (var parameter in typeParameters)
        {
            json.WriteStartObject();
            json.WriteString("name", parameter.Name);
            json.WriteString("variance", parameter.Variance switch
            {
                VarianceKind.In => "in",
                VarianceKind.Out => "out",
                _ => "none",
            });

            json.WriteStartObject("constraints");
            json.WriteBoolean("referenceType", parameter.HasReferenceTypeConstraint);
            json.WriteBoolean("valueType", parameter.HasValueTypeConstraint);
            json.WriteBoolean("notNull", parameter.HasNotNullConstraint);
            json.WriteBoolean("unmanaged", parameter.HasUnmanagedTypeConstraint);
            json.WriteBoolean("constructor", parameter.HasConstructorConstraint);
            json.WriteBoolean("allowsRefLike", AllowsRefLike(parameter));

            json.WriteStartArray("types");
            foreach (var constraint in parameter.ConstraintTypes)
            {
                _types.Write(json, constraint);
            }

            json.WriteEndArray();
            json.WriteEndObject();
            json.WriteEndObject();
        }

        json.WriteEndArray();
    }

    /// <summary>Whether the parameter carries C# 13's <c>allows ref struct</c>.</summary>
    /// <remarks>
    /// Accessed reflectively for the same reason as the extension receiver: the
    /// property is recent, and its absence should read as "no such constraint"
    /// rather than break the whole extractor.
    /// </remarks>
    private static bool AllowsRefLike(ITypeParameterSymbol parameter)
    {
        var property = typeof(ITypeParameterSymbol).GetProperty(
            "AllowsRefLikeType", BindingFlags.Public | BindingFlags.Instance);

        return property?.GetValue(parameter) is true;
    }

    private void WriteParameters(
        Utf8JsonWriter json, string property, IReadOnlyList<IParameterSymbol> parameters)
    {
        json.WriteStartArray(property);
        foreach (var parameter in parameters)
        {
            json.WriteStartObject();
            json.WriteString("name", parameter.Name);
            json.WritePropertyName("type");
            _types.Write(json, parameter.Type);

            json.WriteString("refKind", parameter.RefKind switch
            {
                RefKind.Ref => "ref",
                RefKind.Out => "out",
                RefKind.In => "in",
                RefKind.RefReadOnlyParameter => "refReadonly",
                _ => "none",
            });

            json.WriteBoolean("isParams", parameter.IsParams);

            var hasDefault = HasExplicitDefault(parameter);
            json.WriteBoolean("hasDefault", hasDefault);

            json.WritePropertyName("default");
            if (hasDefault)
            {
                json.WriteStringValue(FormatConstant(parameter.ExplicitDefaultValue));
            }
            else
            {
                json.WriteNullValue();
            }

            json.WriteBoolean("scoped", parameter.ScopedKind != ScopedKind.None);
            WriteAttributes(json, parameter.GetAttributes());
            json.WriteEndObject();
        }

        json.WriteEndArray();
    }

    /// <summary>
    /// Whether the parameter has a default the extractor can read.
    /// </summary>
    /// <remarks>
    /// <c>HasExplicitDefaultValue</c> throws rather than returning false for
    /// some symbols, so the query itself has to be guarded; treating a throw as
    /// "no default" is accurate for the caller and keeps one odd parameter from
    /// failing the whole extraction.
    /// </remarks>
    private static bool HasExplicitDefault(IParameterSymbol parameter)
    {
        try
        {
            return parameter.HasExplicitDefaultValue;
        }
        catch (InvalidOperationException)
        {
            return false;
        }
    }

    private static void WriteAttributes(
        Utf8JsonWriter json, IReadOnlyList<AttributeData> attributes)
    {
        json.WriteStartArray("attributes");
        foreach (var attribute in attributes)
        {
            json.WriteStartObject();
            json.WriteString(
                "type",
                attribute.AttributeClass is { } cls
                    ? TypeSigWriter.MetadataFqn(cls)
                    : "<unknown>");

            json.WriteStartArray("args");
            foreach (var argument in attribute.ConstructorArguments)
            {
                json.WriteStringValue(argument.ToCSharpString());
            }

            json.WriteEndArray();

            json.WriteStartObject("named");
            foreach (var (name, value) in attribute.NamedArguments)
            {
                json.WriteString(name, value.ToCSharpString());
            }

            json.WriteEndObject();
            json.WriteEndObject();
        }

        json.WriteEndArray();
    }

    private static void WriteDeprecated(
        Utf8JsonWriter json, IReadOnlyList<AttributeData> attributes)
    {
        var obsolete = attributes.FirstOrDefault(
            a => a.AttributeClass?.Name == "ObsoleteAttribute");

        json.WritePropertyName("deprecated");
        if (obsolete is null)
        {
            json.WriteNullValue();
            return;
        }

        json.WriteStartObject();

        var arguments = obsolete.ConstructorArguments;
        json.WritePropertyName("message");
        if (arguments.Length > 0 && arguments[0].Value is string message)
        {
            json.WriteStringValue(message);
        }
        else
        {
            json.WriteNullValue();
        }

        json.WriteBoolean(
            "isError", arguments.Length > 1 && arguments[1].Value is true);

        json.WriteEndObject();
    }

    /// <summary>Whether <c>[EditorBrowsable(Never)]</c> is present.</summary>
    private static bool IsHidden(IReadOnlyList<AttributeData> attributes)
    {
        foreach (var attribute in attributes)
        {
            if (attribute.AttributeClass?.Name != "EditorBrowsableAttribute")
            {
                continue;
            }

            // EditorBrowsableState.Never == 1.
            if (attribute.ConstructorArguments.Length > 0
                && attribute.ConstructorArguments[0].Value is int state
                && state == 1)
            {
                return true;
            }
        }

        return false;
    }

    private static void WriteDocs(Utf8JsonWriter json, SymbolDocs docs)
    {
        json.WritePropertyName("doc");
        if (docs.HasXml)
        {
            json.WriteStringValue(docs.Xml);
        }
        else
        {
            json.WriteNullValue();
        }

        json.WriteBoolean("docInherited", docs.Inherited);

        json.WritePropertyName("docLinks");
        if (docs.CrefTargets.Count == 0)
        {
            json.WriteNullValue();
            return;
        }

        json.WriteStartObject();
        foreach (var cref in docs.CrefTargets)
        {
            // Roslyn already rewrote each cref into its documentation-comment
            // id while binding, so the cref *is* the link target; the map is an
            // identity by construction rather than by accident.
            json.WriteString(cref, cref);
        }

        json.WriteEndObject();
    }

    // ── Small helpers ────────────────────────────────────────────────────────

    /// <summary>
    /// The Roslyn documentation-comment id — the producer's whole id space.
    /// </summary>
    /// <remarks>
    /// Roslyn returns null for symbols that have no id form (an unspeakable
    /// name, for instance). The empty string is the schema's "no stable key"
    /// signal, which the Rust side already handles by falling back to a
    /// synthetic member key.
    /// </remarks>
    private static string DocId(ISymbol symbol) =>
        DocumentationCommentId.CreateDeclarationId(symbol) ?? string.Empty;

    /// <summary>The type's own name without namespace, arity, or nesting.</summary>
    private static string SimpleName(INamedTypeSymbol type) => type.Name;

    private static string AccessibilityToken(Accessibility accessibility) => accessibility switch
    {
        Accessibility.Public => "public",
        Accessibility.Protected => "protected",
        Accessibility.Internal => "internal",
        Accessibility.ProtectedOrInternal => "protectedInternal",
        Accessibility.ProtectedAndInternal => "privateProtected",
        Accessibility.Private => "private",
        _ => string.Empty,
    };

    /// <summary>Render a compile-time constant the way C# source would spell it.</summary>
    private static string FormatConstant(object? value) =>
        SymbolDisplay.FormatPrimitive(value!, quoteStrings: true, useHexadecimalNumbers: false)
        ?? "null";

    private static string RoslynVersion()
    {
        var assembly = typeof(CSharpCompilation).Assembly;

        var informational = assembly
            .GetCustomAttribute<AssemblyInformationalVersionAttribute>()
            ?.InformationalVersion;

        if (!string.IsNullOrEmpty(informational))
        {
            // The informational version carries a `+<commit sha>` suffix.
            var plus = informational.IndexOf('+');
            return plus >= 0 ? informational[..plus] : informational;
        }

        return assembly.GetName().Version?.ToString() ?? "unknown";
    }
}
