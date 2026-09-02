using System.Reflection.Metadata;
using System.Text;
using System.Text.Json;
using Microsoft.CodeAnalysis;

namespace Nudox.Oracle;

/// <summary>
/// Writes a Roslyn <see cref="ITypeSymbol"/> as the recursive <c>TypeSig</c>
/// node defined in <c>../src/schema.rs</c>.
/// </summary>
/// <remarks>
/// <para>
/// <b>Property names are load-bearing.</b> <c>TypeSig</c> is a serde
/// internally-tagged enum: <c>#[serde(tag = "kind", rename_all = "camelCase")]</c>
/// renames the <i>variants</i> only. Serde does not rewrite the fields inside a
/// variant unless <c>rename_all_fields</c> is present, and it is not — so
/// <c>type_kind</c>, <c>owner_kind</c>, <c>call_conv</c> and
/// <c>unmanaged_call_convs</c> are emitted in snake_case while every
/// surrounding struct uses camelCase. That asymmetry is intentional here
/// because it mirrors the Rust side exactly; changing it silently drops the
/// field on deserialization.
/// </para>
/// <para>
/// <b>The name join.</b> <see cref="MetadataFqn"/> produces the same string for
/// a type <i>use</i> here as <c>TypeDecl.qualifiedName</c> does for that type's
/// <i>declaration</i>. That identity is what lets the Rust lowering resolve a
/// named type to <c>Type::Nominal</c> instead of degrading it to
/// <c>Type::Any</c>; the two must be produced by this one function.
/// </para>
/// </remarks>
internal sealed class TypeSigWriter
{
    /// <summary>Mirrors the Rust lowering's own recursion bound.</summary>
    private const int MaxDepth = 64;

    /// <summary>Count of unresolvable types met, for <c>diagnostics.errorTypeCount</c>.</summary>
    public int ErrorTypeCount { get; private set; }

    public void Write(Utf8JsonWriter json, ITypeSymbol type) => Write(json, type, 0);

    private void Write(Utf8JsonWriter json, ITypeSymbol type, int depth)
    {
        if (depth > MaxDepth)
        {
            // Matching the Rust bound keeps a pathological type from producing a
            // document the lowering would silently truncate anyway.
            json.WriteStartObject();
            json.WriteString("kind", "error");
            json.WriteString("name", "<recursion-limit>");
            json.WriteEndObject();
            return;
        }

        switch (type)
        {
            case IErrorTypeSymbol error:
                ErrorTypeCount++;
                json.WriteStartObject();
                json.WriteString("kind", "error");
                json.WriteString("name", ErrorName(error));
                json.WriteEndObject();
                return;

            case IDynamicTypeSymbol:
                json.WriteStartObject();
                json.WriteString("kind", "dynamic");
                json.WriteEndObject();
                return;

            case IArrayTypeSymbol array:
                json.WriteStartObject();
                json.WriteString("kind", "array");
                json.WritePropertyName("element");
                Write(json, array.ElementType, depth + 1);
                json.WriteNumber("rank", array.Rank);
                json.WriteString("nullable", Nullable(array));
                json.WriteEndObject();
                return;

            case IPointerTypeSymbol pointer:
                json.WriteStartObject();
                json.WriteString("kind", "pointer");
                json.WritePropertyName("pointee");
                Write(json, pointer.PointedAtType, depth + 1);
                json.WriteEndObject();
                return;

            case IFunctionPointerTypeSymbol funcPtr:
                WriteFunctionPointer(json, funcPtr, depth);
                return;

            case ITypeParameterSymbol typeParam:
                json.WriteStartObject();
                json.WriteString("kind", "typeParam");
                json.WriteString("name", typeParam.Name);
                json.WriteString(
                    "owner_kind",
                    typeParam.TypeParameterKind == TypeParameterKind.Method ? "method" : "type");
                json.WriteString("nullable", Nullable(typeParam));
                json.WriteEndObject();
                return;

            case INamedTypeSymbol named:
                WriteNamed(json, named, depth);
                return;

            default:
                // ITypeSymbol is not sealed and Roslyn does add cases; recording
                // the display string keeps the fact rather than dropping it.
                ErrorTypeCount++;
                json.WriteStartObject();
                json.WriteString("kind", "error");
                json.WriteString("name", type.ToDisplayString());
                json.WriteEndObject();
                return;
        }
    }

    private void WriteFunctionPointer(
        Utf8JsonWriter json, IFunctionPointerTypeSymbol funcPtr, int depth)
    {
        var signature = funcPtr.Signature;

        json.WriteStartObject();
        json.WriteString("kind", "funcPtr");

        json.WriteStartArray("params");
        foreach (var parameter in signature.Parameters)
        {
            Write(json, parameter.Type, depth + 1);
        }

        json.WriteEndArray();

        json.WritePropertyName("return");
        if (signature.ReturnsVoid)
        {
            json.WriteNullValue();
        }
        else
        {
            Write(json, signature.ReturnType, depth + 1);
        }

        var isManaged = signature.CallingConvention == SignatureCallingConvention.Default;
        json.WriteString("call_conv", isManaged ? "managed" : "unmanaged");

        json.WriteStartArray("unmanaged_call_convs");
        if (!signature.UnmanagedCallingConventionTypes.IsDefaultOrEmpty)
        {
            foreach (var conv in signature.UnmanagedCallingConventionTypes)
            {
                // Roslyn models `delegate* unmanaged[Cdecl]` as the marker type
                // `System.Runtime.CompilerServices.CallConvCdecl`; the schema
                // wants the bare convention name.
                var name = conv.Name;
                json.WriteStringValue(
                    name.StartsWith("CallConv", StringComparison.Ordinal)
                        ? name["CallConv".Length..]
                        : name);
            }
        }
        else if (!isManaged)
        {
            // An unmanaged pointer with no explicit modifier still has a
            // convention — the platform default. Recording nothing here would
            // make it indistinguishable from a managed pointer downstream.
            var fallback = signature.CallingConvention switch
            {
                SignatureCallingConvention.CDecl => "Cdecl",
                SignatureCallingConvention.StdCall => "StdCall",
                SignatureCallingConvention.ThisCall => "ThisCall",
                SignatureCallingConvention.FastCall => "FastCall",
                _ => null,
            };
            if (fallback is not null)
            {
                json.WriteStringValue(fallback);
            }
        }

        json.WriteEndArray();
        json.WriteEndObject();
    }

    private void WriteNamed(Utf8JsonWriter json, INamedTypeSymbol named, int depth)
    {
        // `int?` is `System.Nullable<int>` in metadata but a distinct schema node:
        // collapsing it into `named` would make a nullable value type
        // indistinguishable from an ordinary generic instantiation.
        if (named.OriginalDefinition.SpecialType == SpecialType.System_Nullable_T
            && named.TypeArguments.Length == 1)
        {
            json.WriteStartObject();
            json.WriteString("kind", "nullableValue");
            json.WritePropertyName("inner");
            Write(json, named.TypeArguments[0], depth + 1);
            json.WriteEndObject();
            return;
        }

        if (named.IsTupleType)
        {
            json.WriteStartObject();
            json.WriteString("kind", "tuple");
            json.WriteStartArray("elements");
            foreach (var element in named.TupleElements)
            {
                json.WriteStartObject();
                // Only an author-written label is API surface; `Item1` is
                // positional and emitting it would invent a name.
                if (element.IsExplicitlyNamedTupleElement)
                {
                    json.WriteString("name", element.Name);
                }
                else
                {
                    json.WriteNull("name");
                }

                json.WritePropertyName("type");
                Write(json, element.Type, depth + 1);
                json.WriteEndObject();
            }

            json.WriteEndArray();
            json.WriteString("nullable", Nullable(named));
            json.WriteEndObject();
            return;
        }

        json.WriteStartObject();
        json.WriteString("kind", "named");
        json.WriteString("name", MetadataFqn(named));

        json.WriteStartArray("args");
        foreach (var arg in named.TypeArguments)
        {
            Write(json, arg, depth + 1);
        }

        json.WriteEndArray();

        // `owner` is emitted only for a type nested in a *generic* type, which
        // is the case the Rust lowering turns into `Type::QualifiedPath`. For a
        // plainly nested type the containing type carries no type arguments, so
        // emitting an owner would throw away the nominal doc-id link for no gain.
        json.WritePropertyName("owner");
        if (named.ContainingType is { IsGenericType: true } owner)
        {
            Write(json, owner, depth + 1);
        }
        else
        {
            json.WriteNullValue();
        }

        json.WriteString("nullable", Nullable(named));
        json.WriteString("type_kind", named.TypeKind.ToString());
        json.WriteEndObject();
    }

    /// <summary>
    /// The 3-state nullability token, emitted only where C# gives it meaning.
    /// </summary>
    /// <remarks>
    /// A value type is annotated <c>NotAnnotated</c> by Roslyn inside an enabled
    /// nullable context, but that says nothing — <c>int</c> was never nullable.
    /// Forwarding it would wrap every primitive in the IR's "declared non-null"
    /// marker and drown the annotation that does carry information.
    /// </remarks>
    private static string Nullable(ITypeSymbol type)
    {
        var meaningful = type.IsReferenceType || type.TypeKind == TypeKind.TypeParameter;
        if (!meaningful)
        {
            return "none";
        }

        return type.NullableAnnotation switch
        {
            NullableAnnotation.Annotated => "annotated",
            NullableAnnotation.NotAnnotated => "notAnnotated",
            _ => "none",
        };
    }

    /// <summary>
    /// The metadata fully-qualified name, arity backticks kept.
    /// </summary>
    /// <remarks>
    /// Nested types are joined with <c>.</c> rather than the CLR's <c>+</c>
    /// because that is what Roslyn's own <c>DocumentationCommentId</c> does, and
    /// the doc-id is the join key on the Rust side. Using <c>+</c> here would
    /// produce names that never match a declaration.
    /// </remarks>
    public static string MetadataFqn(INamedTypeSymbol type)
    {
        var builder = new StringBuilder();
        AppendFqn(builder, type);
        return builder.ToString();
    }

    private static void AppendFqn(StringBuilder builder, INamedTypeSymbol type)
    {
        if (type.ContainingType is { } containing)
        {
            AppendFqn(builder, containing);
            builder.Append('.');
        }
        else if (type.ContainingNamespace is { IsGlobalNamespace: false } ns)
        {
            builder.Append(NamespaceName(ns));
            builder.Append('.');
        }

        builder.Append(type.MetadataName);
    }

    /// <summary>The dotted name of a namespace.</summary>
    public static string NamespaceName(INamespaceSymbol ns)
    {
        if (ns.IsGlobalNamespace)
        {
            return string.Empty;
        }

        var parts = new List<string>();
        for (var current = ns; current is { IsGlobalNamespace: false }; current = current.ContainingNamespace)
        {
            parts.Add(current.Name);
        }

        parts.Reverse();
        return string.Join('.', parts);
    }

    /// <summary>
    /// The best available name for a type Roslyn could not bind.
    /// </summary>
    /// <remarks>
    /// An unresolved type still has a source spelling, and that spelling is the
    /// only evidence a consumer has of what the package meant. Returning a
    /// placeholder instead would erase it.
    /// </remarks>
    private static string ErrorName(IErrorTypeSymbol error)
    {
        if (!string.IsNullOrEmpty(error.Name))
        {
            return error.MetadataName;
        }

        var display = error.ToDisplayString();
        return string.IsNullOrEmpty(display) ? "<unknown>" : display;
    }
}
