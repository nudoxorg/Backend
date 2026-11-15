using System.Text.Json;

namespace Nudox.Oracle;

/// <summary>One compile-time package assembly selected by a project's restore.</summary>
internal sealed record RestoredPackageReference(string Name, string Path, Version AssemblyVersion, string Project);

/// <summary>
/// The compile-time package assemblies recorded by the restore of every project
/// under the package roots, read from <c>obj/project.assets.json</c> only.
/// </summary>
/// <remarks>
/// Nothing is evaluated or executed: no MSBuild, no package target or props, no
/// directory scan of the NuGet cache. An assembly is a reference only when the
/// restore graph names it as a <c>compile</c> asset of a <c>package</c> library
/// and the exact file exists beneath one of the recorded package folders. Anything
/// that cannot be established is recorded in <see cref="Notes"/> rather than
/// guessed, so the compilation keeps reporting the types it cannot bind.
/// </remarks>
internal sealed class RestoredPackages
{
    private const long MaxAssetsBytes = 64L * 1024 * 1024;
    private const int MaxProjects = 64;
    private const int MaxAssemblies = 4096;

    public static readonly RestoredPackages None = new([], [], 0, 0, new HashSet<string>());

    private RestoredPackages(
        IReadOnlyList<RestoredPackageReference> references,
        IReadOnlyList<string> notes,
        int projectCount,
        int restoredProjectCount,
        IReadOnlySet<string> unrestoredProjects)
    {
        References = references;
        Notes = notes;
        ProjectCount = projectCount;
        RestoredProjectCount = restoredProjectCount;
        UnrestoredProjects = unrestoredProjects;
    }

    public IReadOnlyList<RestoredPackageReference> References { get; }

    /// <summary>Why a project contributed less than its declarations ask for.</summary>
    public IReadOnlyList<string> Notes { get; }

    public int ProjectCount { get; }

    public int RestoredProjectCount { get; }

    /// <summary>Root-relative paths of projects that declare packages but have no usable restore.</summary>
    public IReadOnlySet<string> UnrestoredProjects { get; }

    /// <summary>Reads the restore result of every non-excluded project beneath the roots.</summary>
    public static RestoredPackages Detect(IReadOnlyList<string> roots)
    {
        var notes = new List<string>();
        var unrestored = new HashSet<string>(StringComparer.Ordinal);
        var chosen = new Dictionary<string, RestoredPackageReference>(StringComparer.OrdinalIgnoreCase);
        var projects = 0;
        var restored = 0;
        foreach (var root in roots)
        {
            if (!Directory.Exists(root)) continue;
            var full = System.IO.Path.GetFullPath(root);
            foreach (var project in Directory
                .EnumerateFiles(full, "*.csproj", SearchOption.AllDirectories)
                .OrderBy(p => p, StringComparer.Ordinal))
            {
                if (SourceLoader.IsExcluded(full, project)) continue;
                if (++projects > MaxProjects)
                {
                    notes.Add($"more than {MaxProjects} projects: the remaining restore results were not read");
                    break;
                }

                var display = System.IO.Path.GetRelativePath(full, project).Replace('\\', '/');
                if (!DeclaresPackages(project))
                {
                    continue;
                }

                var outcome = ReadProject(project, display, chosen, notes);
                if (outcome)
                {
                    restored++;
                }
                else
                {
                    unrestored.Add(display);
                }
            }
        }

        var references = chosen.Values
            .OrderBy(reference => reference.Name, StringComparer.OrdinalIgnoreCase)
            .ToList();
        return new RestoredPackages(references, notes, projects, restored, unrestored);
    }

    /// <summary>Root-relative path of the project directory owning <paramref name="relativeFile"/>, if any.</summary>
    public static string? OwningProject(string root, string relativeFile)
    {
        var full = System.IO.Path.GetFullPath(root);
        var directory = System.IO.Path.GetDirectoryName(System.IO.Path.GetFullPath(System.IO.Path.Combine(full, relativeFile)));
        while (directory is not null && directory.StartsWith(full, StringComparison.Ordinal))
        {
            string[] projects;
            try
            {
                projects = Directory.GetFiles(directory, "*.csproj", SearchOption.TopDirectoryOnly);
            }
            catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
            {
                return null;
            }

            if (projects.Length > 0)
            {
                Array.Sort(projects, StringComparer.Ordinal);
                return System.IO.Path.GetRelativePath(full, projects[0]).Replace('\\', '/');
            }

            directory = System.IO.Path.GetDirectoryName(directory);
        }

        return null;
    }

    private static bool DeclaresPackages(string project)
    {
        try
        {
            using var reader = System.Xml.XmlReader.Create(project, new System.Xml.XmlReaderSettings
            {
                DtdProcessing = System.Xml.DtdProcessing.Prohibit,
                XmlResolver = null,
            });
            var document = System.Xml.Linq.XDocument.Load(reader);
            return document.Descendants("PackageReference").Any()
                || document.Descendants("ProjectReference").Any();
        }
        catch (Exception ex) when (ex is System.Xml.XmlException or IOException or UnauthorizedAccessException)
        {
            return false;
        }
    }

    /// <summary>Adds this project's package compile assets; false when no usable restore exists.</summary>
    private static bool ReadProject(
        string project,
        string display,
        Dictionary<string, RestoredPackageReference> chosen,
        List<string> notes)
    {
        var assetsPath = System.IO.Path.Combine(System.IO.Path.GetDirectoryName(project)!, "obj", "project.assets.json");
        FileInfo info;
        try
        {
            info = new FileInfo(assetsPath);
            if (!info.Exists || info.Attributes.HasFlag(FileAttributes.ReparsePoint) || info.LinkTarget is not null)
            {
                notes.Add($"{display} has no obj/project.assets.json: run dotnet restore for this project");
                return false;
            }

            if (info.Length > MaxAssetsBytes)
            {
                notes.Add($"{display}: obj/project.assets.json exceeds the bounded restore size");
                return false;
            }
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            notes.Add($"{display}: obj/project.assets.json could not be read");
            return false;
        }

        JsonDocument document;
        try
        {
            document = JsonDocument.Parse(File.ReadAllBytes(assetsPath));
        }
        catch (Exception ex) when (ex is JsonException or IOException or UnauthorizedAccessException)
        {
            notes.Add($"{display}: obj/project.assets.json is not valid restore output");
            return false;
        }

        using (document)
        {
            var rootElement = document.RootElement;
            if (rootElement.ValueKind != JsonValueKind.Object
                || !rootElement.TryGetProperty("packageFolders", out var folders) || folders.ValueKind != JsonValueKind.Object
                || !rootElement.TryGetProperty("targets", out var targets) || targets.ValueKind != JsonValueKind.Object
                || !rootElement.TryGetProperty("libraries", out var libraries) || libraries.ValueKind != JsonValueKind.Object)
            {
                notes.Add($"{display}: obj/project.assets.json lacks a restore graph: run dotnet restore for this project");
                return false;
            }

            var packageFolders = new List<string>();
            foreach (var folder in folders.EnumerateObject())
            {
                var name = folder.Name;
                if (!IsNativePath(name))
                {
                    notes.Add($"{display} was restored on another operating system ({name}): restore it again with this machine's SDK in a separate working copy");
                    return false;
                }

                packageFolders.Add(System.IO.Path.GetFullPath(name));
            }

            // Only the runtime-independent graph decides compile references.
            var graphs = targets.EnumerateObject().Where(target => !target.Name.Contains('/')).ToList();
            if (graphs.Count != 1)
            {
                notes.Add($"{display} restores {graphs.Count} target frameworks: package references of a multi-targeted project are not read");
                return false;
            }

            foreach (var entry in graphs[0].Value.EnumerateObject())
            {
                if (entry.Value.ValueKind != JsonValueKind.Object
                    || !entry.Value.TryGetProperty("type", out var kind) || kind.GetString() != "package"
                    || !entry.Value.TryGetProperty("compile", out var compile) || compile.ValueKind != JsonValueKind.Object
                    || !libraries.TryGetProperty(entry.Name, out var library)
                    || !library.TryGetProperty("path", out var libraryPath) || libraryPath.GetString() is not { } packagePath
                    || !IsRelativeAssetPath(packagePath))
                {
                    continue;
                }

                foreach (var asset in compile.EnumerateObject())
                {
                    var relative = asset.Name;
                    if (!relative.EndsWith(".dll", StringComparison.OrdinalIgnoreCase) || !IsRelativeAssetPath(relative))
                    {
                        continue;
                    }

                    var file = Locate(packageFolders, packagePath, relative);
                    if (file is null)
                    {
                        notes.Add($"{display}: package {entry.Name} asset {relative} is not in the NuGet package folder: run dotnet restore for this project");
                        continue;
                    }

                    Version version;
                    string name;
                    try
                    {
                        var identity = System.Reflection.AssemblyName.GetAssemblyName(file);
                        name = identity.Name ?? System.IO.Path.GetFileNameWithoutExtension(file);
                        version = identity.Version ?? new Version(0, 0, 0, 0);
                    }
                    catch (Exception ex) when (ex is BadImageFormatException or IOException or UnauthorizedAccessException)
                    {
                        notes.Add($"{display}: package {entry.Name} asset {relative} is not a managed assembly");
                        continue;
                    }

                    var candidate = new RestoredPackageReference(name, file, version, display);
                    if (chosen.TryGetValue(name, out var existing))
                    {
                        // One compilation holds every project, so one copy of an assembly name
                        // must win: the highest assembly version, then the lowest path.
                        var replaces = version > existing.AssemblyVersion
                            || (version == existing.AssemblyVersion
                                && string.CompareOrdinal(file, existing.Path) < 0);
                        if (!replaces) continue;
                    }
                    else if (chosen.Count >= MaxAssemblies)
                    {
                        notes.Add($"more than {MaxAssemblies} package assemblies: the rest were not read");
                        return true;
                    }

                    chosen[name] = candidate;
                }
            }
        }

        return true;
    }

    private static string? Locate(IReadOnlyList<string> folders, string packagePath, string asset)
    {
        foreach (var folder in folders)
        {
            var candidate = System.IO.Path.GetFullPath(System.IO.Path.Combine(
                folder,
                packagePath.Replace('/', System.IO.Path.DirectorySeparatorChar),
                asset.Replace('/', System.IO.Path.DirectorySeparatorChar)));
            var prefix = folder.EndsWith(System.IO.Path.DirectorySeparatorChar) ? folder : folder + System.IO.Path.DirectorySeparatorChar;
            if (!candidate.StartsWith(prefix, StringComparison.Ordinal))
            {
                continue;
            }

            try
            {
                var info = new FileInfo(candidate);
                if (info.Exists && info.LinkTarget is null && !info.Attributes.HasFlag(FileAttributes.ReparsePoint))
                {
                    return candidate;
                }
            }
            catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
            {
                // An unreadable candidate is simply not found in this folder.
            }
        }

        return null;
    }

    private static bool IsRelativeAssetPath(string path)
    {
        if (path.Length == 0 || path.Length > 1024 || path.Contains('\\') || path.StartsWith('/') || path.Contains('\0'))
        {
            return false;
        }

        return path.Split('/').All(segment => segment.Length > 0 && segment != "." && segment != "..");
    }

    private static bool IsNativePath(string path)
        // A restore made on another operating system records folders in that system's
        // syntax (for example a drive letter), which is not rooted here.
        => path.Length > 0 && !path.Contains('\0') && System.IO.Path.IsPathRooted(path);
}
