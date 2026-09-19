using System.Xml.Linq;
using Microsoft.CodeAnalysis;

namespace Nudox.Oracle;

/// <summary>The documentation facts carried for one symbol.</summary>
/// <param name="Xml">
/// The raw doc XML, still wrapped in Roslyn's <c>&lt;member name="…"&gt;</c>
/// element. The Rust <c>xmldoc</c> module strips that wrapper itself, so
/// unwrapping here would just move the same work and lose the id attribute.
/// </param>
/// <param name="Inherited">
/// True when the text came from an ancestor because the symbol's own comment
/// was <c>&lt;inheritdoc/&gt;</c>.
/// </param>
/// <param name="CrefTargets">
/// Every <c>cref</c> mentioned anywhere in the comment, in document order and
/// de-duplicated. These become the symbol's doc links.
/// </param>
/// <param name="ExceptionTargets">The exception cref targets in document order.</param>
internal readonly record struct SymbolDocs(
    string? Xml,
    bool Inherited,
    IReadOnlyList<string> CrefTargets,
    IReadOnlyList<string> ExceptionTargets)
{
    public static readonly SymbolDocs None = new(null, false, [], []);

    public bool HasXml => !string.IsNullOrWhiteSpace(Xml);
}

/// <summary>
/// Retrieves and normalises C# XML documentation comments.
/// </summary>
/// <remarks>
/// Roslyn resolves <c>&lt;include&gt;</c> when asked but deliberately leaves
/// <c>&lt;inheritdoc/&gt;</c> alone — it is a documentation-tool convention, not
/// a language feature. Resolving it here is what makes the schema's
/// <c>docInherited</c> flag mean something: without it, every overriding member
/// in a well-documented package arrives with a comment whose entire content is
/// the word "inheritdoc".
/// </remarks>
internal static class DocComments
{
    /// <summary>How far up an inheritance chain to look for inherited text.</summary>
    /// <remarks>
    /// Deep hierarchies are legitimate, but a cycle through interfaces is not
    /// impossible to construct; the bound makes termination obvious rather than
    /// relying on the visited set alone.
    /// </remarks>
    private const int MaxInheritDepth = 16;

    public static SymbolDocs For(ISymbol symbol)
    {
        var xml = symbol.GetDocumentationCommentXml(expandIncludes: true);

        if (string.IsNullOrWhiteSpace(xml))
        {
            return SymbolDocs.None;
        }

        if (!ContainsInheritDoc(xml))
        {
            return new SymbolDocs(xml, false, Crefs(xml), Exceptions(xml));
        }

        foreach (var ancestor in Ancestors(symbol))
        {
            var inherited = ancestor.GetDocumentationCommentXml(expandIncludes: true);
            if (!string.IsNullOrWhiteSpace(inherited) && !ContainsInheritDoc(inherited))
            {
                return new SymbolDocs(inherited, true, Crefs(inherited), Exceptions(inherited));
            }
        }

        // The marker was present but nothing up the chain had text. Keeping the
        // original (rather than returning null) preserves any sibling tags the
        // author wrote alongside the marker, and `Inherited` stays true so a
        // consumer can tell this apart from an ordinary comment.
        return new SymbolDocs(xml, true, Crefs(xml), Exceptions(xml));
    }

    private static bool ContainsInheritDoc(string xml) =>
        xml.Contains("<inheritdoc", StringComparison.OrdinalIgnoreCase);

    /// <summary>
    /// Symbols whose documentation an <c>&lt;inheritdoc/&gt;</c> may draw from,
    /// nearest first.
    /// </summary>
    /// <remarks>
    /// The order follows the convention documentation tools use: an override
    /// takes its base member's text, and a member that implements an interface
    /// takes the interface member's text.
    /// </remarks>
    private static IEnumerable<ISymbol> Ancestors(ISymbol symbol)
    {
        var seen = new HashSet<ISymbol>(SymbolEqualityComparer.Default) { symbol };
        var depth = 0;

        foreach (var candidate in Candidates(symbol))
        {
            if (depth++ >= MaxInheritDepth)
            {
                yield break;
            }

            if (seen.Add(candidate))
            {
                yield return candidate;
            }
        }
    }

    private static IEnumerable<ISymbol> Candidates(ISymbol symbol)
    {
        switch (symbol)
        {
            case IMethodSymbol method:
                for (var b = method.OverriddenMethod; b is not null; b = b.OverriddenMethod)
                {
                    yield return b;
                }

                foreach (var explicitImpl in method.ExplicitInterfaceImplementations)
                {
                    yield return explicitImpl;
                }

                foreach (var implicitImpl in ImplementedInterfaceMembers(method))
                {
                    yield return implicitImpl;
                }

                break;

            case IPropertySymbol property:
                for (var b = property.OverriddenProperty; b is not null; b = b.OverriddenProperty)
                {
                    yield return b;
                }

                foreach (var explicitImpl in property.ExplicitInterfaceImplementations)
                {
                    yield return explicitImpl;
                }

                foreach (var implicitImpl in ImplementedInterfaceMembers(property))
                {
                    yield return implicitImpl;
                }

                break;

            case IEventSymbol evt:
                for (var b = evt.OverriddenEvent; b is not null; b = b.OverriddenEvent)
                {
                    yield return b;
                }

                foreach (var explicitImpl in evt.ExplicitInterfaceImplementations)
                {
                    yield return explicitImpl;
                }

                foreach (var implicitImpl in ImplementedInterfaceMembers(evt))
                {
                    yield return implicitImpl;
                }

                break;

            case INamedTypeSymbol type:
                for (var b = type.BaseType; b is not null; b = b.BaseType)
                {
                    yield return b;
                }

                foreach (var iface in type.AllInterfaces)
                {
                    yield return iface;
                }

                break;
        }
    }

    /// <summary>Interface members this member implements implicitly.</summary>
    private static IEnumerable<ISymbol> ImplementedInterfaceMembers(ISymbol member)
    {
        var containing = member.ContainingType;
        if (containing is null)
        {
            yield break;
        }

        foreach (var iface in containing.AllInterfaces)
        {
            foreach (var candidate in iface.GetMembers(member.Name))
            {
                var implementation = containing.FindImplementationForInterfaceMember(candidate);
                if (implementation is not null
                    && SymbolEqualityComparer.Default.Equals(implementation, member))
                {
                    yield return candidate;
                }
            }
        }
    }

    /// <summary>
    /// Every <c>cref</c> in the comment, de-duplicated, in document order.
    /// </summary>
    /// <remarks>
    /// Roslyn rewrites a source cref into its documentation-comment id
    /// (<c>T:System.String</c>) while binding, so these values are already the
    /// join keys the Rust lowering uses for links. A cref Roslyn could not bind
    /// is emitted by the compiler as <c>!:Text</c>; those are kept rather than
    /// filtered, because "this package documents a link that does not resolve"
    /// is a fact about the package and dropping it here would hide it.
    /// </remarks>
    private static IReadOnlyList<string> Crefs(string xml)
    {
        XDocument document;
        try
        {
            document = XDocument.Parse(xml, LoadOptions.PreserveWhitespace);
        }
        catch (System.Xml.XmlException)
        {
            // A malformed comment is common in the wild; the prose is still
            // usable, so losing only the links is the right degradation.
            return [];
        }

        var targets = new List<string>();
        var seen = new HashSet<string>(StringComparer.Ordinal);

        foreach (var attribute in document.Descendants().Attributes("cref"))
        {
            var value = attribute.Value;
            if (!string.IsNullOrWhiteSpace(value) && seen.Add(value))
            {
                targets.Add(value);
            }
        }

        return targets;
    }

    private static IReadOnlyList<string> Exceptions(string xml)
    {
        try
        {
            return XDocument.Parse(xml, LoadOptions.PreserveWhitespace)
                .Descendants("exception")
                .Attributes("cref")
                .Select(attribute => attribute.Value)
                .Where(value => !string.IsNullOrWhiteSpace(value))
                .Distinct(StringComparer.Ordinal)
                .ToArray();
        }
        catch (System.Xml.XmlException)
        {
            return [];
        }
    }
}
