// MARK: - Cargo Package Registry

import Foundation
import Logging
import SemVer
import SwiftGitX
import SwiftSoup
import zlib

public struct CargoRegistry: PackageRegistry {
  private let logger: Logger
  private let httpClient: HTTPRequesting

  public init(logger: Logger = Logger(label: "CargoRegistry")) {
    self.logger = logger
    self.httpClient = HTTPRequest(logger: logger)
  }

  public func search_packages(for query: String) async -> Result<[Package], NewDocsError> {
    do {
      let encodedQuery =
        query.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? query
      let response = try await httpClient.request(
        "https://crates.io/api/v1/crates?q=\(encodedQuery)")

      guard response.isSuccess else {
        return .failure(.networkError("Failed to search crates: \(response.statusCode)"))
      }

      let json = try response.asJSON()
      guard let crates = json["crates"] as? [[String: Any]] else {
        return .failure(.parsingError("Invalid crates.io response format"))
      }

      let packages = crates.compactMap { crateData -> Package? in
        guard let name = crateData["name"] as? String,
          let description = crateData["description"] as? String,
          let source_string = crateData["repository"] as? String
        else { return nil }
        return CargoPackage(
          slug: name, name: name, description: description, source: URL(string: source_string)!)
      }

      return .success(packages)
    } catch {
      return .failure(.networkError(error.localizedDescription))
    }
  }

  public func get_package(UUID: UInt64) async -> Result<Package, NewDocsError> {
    // For Cargo, we don't use UUIDs - this would need a mapping system
    return .failure(.invalidEntry("UUID-based lookup not supported for Cargo packages"))
  }

  public func get_package(named: String) async -> Result<[Package], NewDocsError> {
    do {
      let response = try await httpClient.request("https://crates.io/api/v1/crates/\(named)")

      guard response.isSuccess else {
        return .failure(.networkError("Failed to fetch crate: \(response.statusCode)"))
      }

      let json = try response.asJSON()
      guard let crateData = json["crate"] as? [String: Any],
        let name = crateData["name"] as? String
      else {
        return .failure(.parsingError("Invalid crate response format"))
      }

      guard let sourceData = json["repository"] as? URL
      else {
        return .failure(.parsingError("Invalid crate response format"))
      }

      let description = crateData["description"] as? String
      let package = CargoPackage(
        slug: name, name: name, description: description, source: sourceData)
      return .success([package])
    } catch {
      return .failure(.networkError(error.localizedDescription))
    }
  }

  public func get_reference() async -> Package {
    return CargoPackage(
      slug: "rust-reference",
      name: "The Rust Reference",
      description: "The Rust Language Reference",
      source: URL(string: "https://doc.rust-lang.org/reference/")!
    )
  }
}

// MARK: - Cargo Package

public struct CargoPackage: Package {
  public var slug: String
  public var name: String
  public var lang: Language = .Rust
  public let UUID: Int64
  public let source: URL

  private let packageDescription: String?
  private let httpClient: HTTPRequesting
  private let logger: Logger

  public init(slug: String, name: String?, description: String? = nil, source: URL) {
    self.slug = slug
    self.name = name ?? slug
    self.packageDescription = description
    self.UUID = Int64(slug.hashValue)
    self.logger = Logger(label: "CargoPackage[\(slug)]")
    self.httpClient = HTTPRequest(logger: logger)
    self.source = source
  }

  public func get_available_versions() async -> Result<[Version], NewDocsError> {
    do {
      let response = try await httpClient.request(
        "https://crates.io/api/v1/crates/\(slug)/versions")

      guard response.isSuccess else {
        return .failure(.networkError("Failed to fetch versions: \(response.statusCode)"))
      }

      let json = try response.asJSON()
      guard let versions = json["versions"] as? [[String: Any]] else {
        return .failure(.parsingError("Invalid versions response"))
      }

      let semverVersions = versions.compactMap { versionData -> Version? in
        guard let versionString = versionData["num"] as? String else { return nil }
        return try? Version(versionString)
      }

      return .success(semverVersions.sorted(by: >))
    } catch {
      return .failure(.networkError(error.localizedDescription))
    }
  }

  public func flags() async -> Result<[String]?, NewDocsError> {
    // Cargo features are package-specific and require parsing Cargo.toml
    // For now, return common Rust feature flags
    return .success(["default", "std", "alloc", "core"])
  }

  public func description() async -> Result<String?, NewDocsError> {
    return .success(packageDescription)
  }

  public func dependencies() async -> Result<[Package], NewDocsError> {
    do {
      let response = try await httpClient.request(
        "https://crates.io/api/v1/crates/\(slug)/dependencies")

      guard response.isSuccess else {
        return .failure(.networkError("Failed to fetch dependencies: \(response.statusCode)"))
      }

      let json = try response.asJSON()
      guard let deps = json["dependencies"] as? [[String: Any]] else {
        return .failure(.parsingError("Invalid dependencies response"))
      }

      let packages = deps.compactMap { depData -> Package? in
        guard let name = depData["crate_id"] as? String else { return nil }
        guard let source = depData["repository"] as? URL else { return nil }
        return CargoPackage(slug: name, name: name, source: source)
      }

      return .success(packages)
    } catch {
      return .failure(.networkError(error.localizedDescription))
    }
  }

  public func dependents() async -> Result<[Package], NewDocsError> {
    do {
      let response = try await httpClient.request(
        "https://crates.io/api/v1/crates/\(slug)/reverse_dependencies")

      guard response.isSuccess else {
        return .failure(.networkError("Failed to fetch dependents: \(response.statusCode)"))
      }

      let json = try response.asJSON()
      guard let deps = json["dependencies"] as? [[String: Any]] else {
        return .failure(.parsingError("Invalid reverse dependencies response"))
      }

      let packages = deps.compactMap { depData -> Package? in
        guard let crateData = depData["crate"] as? [String: Any],
          let name = crateData["name"] as? String,
          let source = crateData["repository"] as? URL
        else { return nil }
        return CargoPackage(slug: name, name: name, source: source)
      }

      return .success(packages)
    } catch {
      return .failure(.networkError(error.localizedDescription))
    }
  }

  public func retrieve(at version: Version, flags: [String]?) async throws -> Documentation {
    return RustDocScraper(
      package: self,
      version: version,
      features: flags ?? []
    )
  }
}

// MARK: - Rust Documentation Scraper
public struct RustDocScraper: Documentation {
  public let logger: Logger
  public let package: CargoPackage
  public let version: Version
  private let httpClient: HTTPRequesting

  public var name: String { package.name }
  public var slug: String { package.slug }
  public var links: [String: URL] {
    if isStandardLibrary {
      return [
        "home": URL(string: "https://www.rust-lang.org/")!,
        "code": URL(string: "https://github.com/rust-lang/rust")!,
      ]
    } else {
      return [
        "home": URL(string: "https://crates.io/crates/\(package.slug)")!,
        "docs": URL(string: "https://docs.rs/\(package.slug)")!,
      ]
    }
  }

  private var isStandardLibrary: Bool {
    package.slug == "std"
  }

  public init(package: CargoPackage, version: Version, features: [String] = []) {
    self.package = package
    self.version = version
    self.logger = Logger(label: "RustDocScraper[\(package.slug)]")
    self.httpClient = HTTPRequest(logger: logger)
  }

  /// This function builds pages
  public func buildPages() async throws -> [DocumentationPage] {
    // Decompress ZSTD
    let out = URL(filePath: "./out.json")

    let bin = Process()
    bin.executableURL =
      URL(
        fileURLWithPath:
          "/Users/philocalyst/.rustup/toolchains/nightly-aarch64-apple-darwin/bin/cargo")

    let arguments = [
      "rustdoc",
      "--package", self.package.name,  // Specify the package you want to document
      "--",  // Separator for rustdoc arguments
      "--document-private-items",
      "--output-format", "json",
      "-Z", "unstable-options",
    ]

    bin.arguments = arguments

    let estimated_location = URL(
      fileURLWithPath: "out/target/doc/\(self.package.name).json",
      relativeTo: URL(fileURLWithPath: FileManager.default.currentDirectoryPath))
    let json = try await getJSON(
      source: self.package.source, command: bin, output_location: estimated_location)

    let crateData = try JSONDecoder().decode(RustdocCrate.self, from: json)

    let entries = try mapCrateToEntries(crateData)
    if let jsonData = try? JSONEncoder().encode(entries),
      let jsonString = String(data: jsonData, encoding: .utf8)
    {
      try jsonData.write(to: out, options: .atomic)
    }

    let page = DocumentationPage(
      path: [slug, "index"],
      internalURLs: [],
      entries: entries
    )

    return [page]
  }

  // MARK: - JSON URL
  private func rustdocJSONURL() -> String {
    if isStandardLibrary {
      return "https://doc.rust-lang.org/nightly/std/std.json"
    } else {
      return "https://docs.rs/crate/\(package.slug)/\(version)/\(package.slug).json"
    }
  }

  // MARK: - Entry Mapping
  private func mapCrateToEntries(_ crate: RustdocCrate) throws -> [Entry] {
    var entries: [Int: Entry] = [:]
    var idToPath: [Int: [String]] = [:]
    var traitImpls: [(implId: Int, impl: Impl)] = []

    // Build path map
    for (id, summary) in crate.paths {
      idToPath[id] = summary.path
    }

    func getPath(for id: Int, name: String?) -> [String] {
      if let p = idToPath[id] { return p }
      if let parent = findParent(of: id, in: crate) {
        let parentPath = getPath(for: parent.id, name: parent.name)
        return parentPath + [name ?? "unnamed"]
      }
      return [name ?? "unnamed"]
    }

    // First pass: create entries
    for (id, item) in crate.index {
      let name = item.name ?? "unnamed"
      let fqPath = getPath(for: id, name: name)
      let visibility = item.visibility
      let docs = item.docs

      var members: [String]? = nil
      var inputParams: [Parameter]? = nil
      var outputParams: [Parameter]? = nil
      let typeParams: [String]? = nil

      switch item.inner {
      case .enumItem(let enumData):
        // Extract variant names for enum members
        members = enumData.variants.compactMap { variantId in
          guard let variantItem = crate.index[variantId] else { return nil }
          return variantItem.name
        }

      case .structItem(let structData):
        // Extract field names for struct members based on kind
        switch structData.kind {
        case .unit:
          members = []

        case .tuple(let fieldIds):
          members = fieldIds.enumerated().compactMap { idx, fieldId in
            guard let fieldId = fieldId else { return nil }
            return "\(idx)"
          }

        case .plain(let fieldIds, _):
          members = fieldIds.compactMap { fieldId in
            crate.index[fieldId]?.name
          }
        }

      case .module(let module):
        members = module.items.compactMap { crate.index[$0]?.name }

      case .function(let fn):
        inputParams = fn.sig.inputs.map { tuple in
          Parameter(
            name: tuple.name,
            type: tuple.type.toType(),
            attributes: nil,
            defaultValue: nil,
            description: nil
          )
        }
        if let output = fn.sig.output {
          outputParams = [
            Parameter(
              name: "return",
              type: output.toType(),
              attributes: nil,
              defaultValue: nil,
              description: nil
            )
          ]
        }

      case .traitItem(let traitData):
        // Collect all trait members: methods, associated types, and constants
        members = traitData.items.compactMap { itemId in
          crate.index[itemId]?.name
        }

      case .impl(let implData):
        // Store impl blocks for second pass
        traitImpls.append((implId: id, impl: implData))
        // For now, collect impl item names
        members = implData.items.compactMap { itemId in
          crate.index[itemId]?.name
        }

      default:
        break
      }

      let entry = try Entry(
        path: fqPath,
        kind: item.inner.toKind(item: item, index: crate.index) ?? Kind.constant,
        visibility: visibility.toVisibility(),
        members: members,
        inputParameters: inputParams,
        outputParameters: outputParams,
        typeParameters: typeParams,
        documentation: docs,
        name: name,
        id: id
      )
      entries[id] = entry
    }

    // Second pass: link associated items and create comprehensive trait/impl entries
    for (id, item) in crate.index {
      switch item.inner {

      // Link impl items to parent type AND create comprehensive impl entries
      case .impl(let implBlock):
        // Link impl members to the type being implemented for
        if let parentId = resolveTypeId(implBlock.for, in: crate),
          var parentEntry = entries[parentId]
        {
          let implMemberNames: [String] = implBlock.items.compactMap { childId in
            if let childItem = crate.index[childId] {
              // Generate synthetic path if missing
              if idToPath[childId] == nil {
                idToPath[childId] = parentEntry.path + [childItem.name ?? "unnamed"]
              }
              return childItem.name
            }
            return nil
          }
          var updatedMembers = parentEntry.members ?? []
          updatedMembers.append(contentsOf: implMemberNames)
          parentEntry = try parentEntry.withMembers(updatedMembers)
          entries[parentId] = parentEntry
        }

        // If this is a trait impl, create a comprehensive TraitImpl entry
        if let traitPath = implBlock.trait,
          let implName = item.name,
          let forType = resolveTypeId(implBlock.for, in: crate)
        {
          let implPath = getPath(for: id, name: implName)

          // Collect implemented methods
          let methods: [DocsFunction] = implBlock.items.compactMap { methodId in
            guard let methodItem = crate.index[methodId],
              case .function(let fn) = methodItem.inner,
              let methodName = methodItem.name
            else {
              return nil
            }

            var attrs: [FunctionAttributes] = []
            if fn.sig.is_c_variadic { attrs.append(.variadic) }
            if fn.header.is_const { attrs.append(.const) }
            if fn.header.is_async { attrs.append(.async) }
            if fn.header.is_unsafe { attrs.append(.unsafe) }

            return DocsFunction(
              inputParameters: fn.sig.inputs.map { input in
                Parameter(
                  name: input.name,
                  type: input.type.toType(),
                  attributes: nil,
                  defaultValue: nil,
                  description: nil
                )
              },
              outputParameters: fn.sig.output.map { outputType in
                [
                  Parameter(
                    name: "return",
                    type: outputType.toType(),
                    attributes: nil,
                    defaultValue: nil,
                    description: nil
                  )
                ]
              },
              attributes: attrs.isEmpty ? nil : attrs,
              generics: fn.generics.toGenerics(),
              name: methodName,
              implemented: fn.has_body,
              visibility: methodItem.visibility.toVisibility()
            )
          }

          // Collect associated type implementations
          let assocTypes: [AssociatedTypeImpl] = implBlock.items.compactMap { itemId in
            guard let implItem = crate.index[itemId],
              case .assocType(_, _, let type) = implItem.inner,
              let assocName = implItem.name,
              let concreteType = type
            else {
              return nil
            }
            return AssociatedTypeImpl(name: assocName, type: concreteType.toType())
          }

          // Collect associated constant implementations
          let assocConstants: [TraitConstant] = implBlock.items.compactMap { itemId in
            guard let implItem = crate.index[itemId],
              case .assocConst(let type, let value) = implItem.inner,
              let constName = implItem.name
            else {
              return nil
            }
            return TraitConstant(
              name: constName,
              type: type.toType(),
              defaultValue: value.map { ConstExpr(expr: $0) },
              docs: implItem.docs
            )
          }

          let traitImpl = TraitImpl(
            trait: TraitRef(name: traitPath.path, args: []),
            forType: implBlock.for.toType(),
            generics: implBlock.generics.toGenerics(),
            whereConstraints: nil,
            methods: methods.isEmpty ? nil : methods,
            associatedTypes: assocTypes.isEmpty ? nil : assocTypes,
            associatedConstants: assocConstants.isEmpty ? nil : assocConstants,
            isNegative: implBlock.is_negative,
            isBlanket: implBlock.blanket_impl != nil,
            isUnsafe: implBlock.is_unsafe,
            visibility: item.visibility.toVisibility(),
            docs: item.docs
          )

          let implEntry = try Entry(
            path: implPath,
            kind: .traitImpl(traitImpl),
            visibility: item.visibility.toVisibility(),
            members: implBlock.items.compactMap { crate.index[$0]?.name },
            inputParameters: nil,
            outputParameters: nil,
            typeParameters: nil,
            documentation: item.docs,
            name: implName,
            id: id
          )
          entries[id] = implEntry
        }

      // Link struct fields
      case .structItem(let structData):
        if var structEntry = entries[id] {
          let fieldNames = structData.kind.fieldIDs().compactMap { crate.index[$0]?.name }
          var updatedMembers = structEntry.members ?? []
          updatedMembers.append(contentsOf: fieldNames)
          structEntry = try structEntry.withMembers(updatedMembers)
          entries[id] = structEntry
        }

      // Link enum variants
      case .enumItem(let enumData):
        if var enumEntry = entries[id] {
          let variantNames = enumData.variants.compactMap { crate.index[$0]?.name }
          var updatedMembers = enumEntry.members ?? []
          updatedMembers.append(contentsOf: variantNames)
          enumEntry = try enumEntry.withMembers(updatedMembers)
          entries[id] = enumEntry
        }

      // Create comprehensive trait definition entries
      case .traitItem(let traitData):
        if var traitEntry = entries[id],
          let traitName = item.name
        {
          // Collect required methods
          let requiredMethods: [TraitMethod] = traitData.items.compactMap { itemId in
            guard let traitItem = crate.index[itemId],
              case .function(let fn) = traitItem.inner,
              let methodName = traitItem.name
            else {
              return nil
            }

            var attrs: [FunctionAttributes] = []
            if fn.sig.is_c_variadic { attrs.append(.variadic) }
            if fn.header.is_const { attrs.append(.const) }
            if fn.header.is_async { attrs.append(.async) }
            if fn.header.is_unsafe { attrs.append(.unsafe) }

            // Determine receiver kind
            let receiver: ReceiverKind?
            if let firstParam = fn.sig.inputs.first,
              firstParam.name == "self"
            {
              switch firstParam.type {
              case .borrowedRef(_, let isMutable, _):
                receiver = isMutable ? .mutRef : .sharedRef
              default:
                receiver = .owned
              }
            } else {
              receiver = .static
            }

            return TraitMethod(
              name: methodName,
              parameters: fn.sig.inputs.map { input in
                Parameter(
                  name: input.name,
                  type: input.type.toType(),
                  attributes: nil,
                  defaultValue: nil,
                  description: nil
                )
              },
              returnType: fn.sig.output?.toType(),
              generics: fn.generics.toGenerics(),
              attributes: attrs.isEmpty ? nil : attrs,
              receiver: receiver,
              hasDefaultImplementation: fn.has_body,
              docs: traitItem.docs
            )
          }

          // Collect associated types
          let assocTypes: [AssociatedType] = traitData.items.compactMap { itemId in
            guard let traitItem = crate.index[itemId],
              case .assocType(let generics, let bounds, let defaultType) = traitItem.inner,
              let assocName = traitItem.name
            else {
              return nil
            }
            return AssociatedType(
              name: assocName,
              bounds: bounds.map { $0.toGenericBound() },
              defaultType: defaultType?.toType(),
              docs: traitItem.docs
            )
          }

          // Collect required constants
          let requiredConstants: [TraitConstant] = traitData.items.compactMap { itemId in
            guard let traitItem = crate.index[itemId],
              case .assocConst(let type, let value) = traitItem.inner,
              let constName = traitItem.name
            else {
              return nil
            }
            return TraitConstant(
              name: constName,
              type: type.toType(),
              defaultValue: value.map { ConstExpr(expr: $0) },
              docs: traitItem.docs
            )
          }

          // Collect supertraits
          let superTraits = traitData.bounds.compactMap { bound -> TraitRef? in
            if case .trait_bound(let trait, _, _) = bound {
              return TraitRef(name: trait.path, args: [])
            }
            return nil
          }

          var attrs: [TraitAttribute] = []
          if traitData.is_auto { attrs.append(.auto) }
          if traitData.is_unsafe { attrs.append(.unsafe) }
          if traitData.is_dyn_compatible { attrs.append(.objectSafe) }

          let traitDef = TraitDef(
            name: traitName,
            generics: traitData.generics.toGenerics(),
            superTraits: superTraits.isEmpty ? nil : superTraits,
            associatedTypes: assocTypes.isEmpty ? nil : assocTypes,
            requiredMethods: requiredMethods.isEmpty ? nil : requiredMethods,
            providedMethods: nil,  // Could separate these based on has_body
            requiredConstants: requiredConstants.isEmpty ? nil : requiredConstants,
            attributes: attrs.isEmpty ? nil : attrs,
            visibility: item.visibility.toVisibility(),
            docs: item.docs
          )

          // Update entry with TraitDef kind
          traitEntry = try Entry(
            path: traitEntry.path,
            kind: .traitDef(traitDef),
            visibility: traitEntry.visibility,
            members: traitEntry.members,
            inputParameters: traitEntry.inputParameters,
            outputParameters: traitEntry.outputParameters,
            typeParameters: traitEntry.typeParameters,
            documentation: traitEntry.documentation,
            name: traitEntry.name,
            id: traitEntry.id
          )
          entries[id] = traitEntry
        }

      // Link union fields
      case .union(let unionData):
        if var unionEntry = entries[id] {
          let fieldNames = unionData.fields.compactMap { crate.index[$0]?.name }
          var updatedMembers = unionEntry.members ?? []
          updatedMembers.append(contentsOf: fieldNames)
          unionEntry = try unionEntry.withMembers(updatedMembers)
          entries[id] = unionEntry
        }

      default:
        break
      }
    }

    return Array(entries.values)
  }

  private func findParent(of id: Int, in crate: RustdocCrate) -> RustdocItem? {
    for (_, item) in crate.index {
      switch item.inner {
      case .module(let m) where m.items.contains(id): return item
      default: continue
      }
    }
    return nil
  }
}

// MARK: - Rust HTML Cleaning Filter

public struct RustCleanHtmlFilter: Filter {
  public func apply(to document: Document, context: FilterContext) throws -> Document {
    let subpath = context.subpath

    // Handle different document types
    if subpath.hasPrefix("book/") || subpath.hasPrefix("reference/") {
      if let content = try document.select("#content main").first() {
        try document.body()?.html(try content.outerHtml())
      }
    } else if subpath == "error-index" {
      try document.select(".error-undescribed").remove()

      for node in try document.select(".error-described").array() {
        let children = node.children()
        try node.before(children.outerHtml())
        try node.remove()
      }
    } else {
      // Standard rustdoc processing
      if let main = try document.select("#main, #main-content").first() {
        try document.body()?.html(try main.outerHtml())
      }

      try document.select(".toggle-wrapper").remove()
      try document.select(".anchor").remove()

      // Fix main headings
      for node in try document.select(".main-heading > h1").array() {
        try node.select("button").remove()
        try node.parent()?.tagName("h1")
        try node.parent()?.text(node.text())
      }

      // Fix stability annotations
      for node in try document.select(".stability .stab").array() {
        try node.tagName("span")
      }
    }

    // Common cleanup
    try document.select(".doc-anchor").remove()

    // Fix notable trait sections
    for node in try document.select(".method, .rust.trait").array() {
      if let traitSection = try node.select(".notable-traits").first() {
        let content = try traitSection.select(".notable-traits-tooltiptext")
        try traitSection.select(".notable-traits-tooltip").remove()
        for contentNode in content.array() {
          try traitSection.appendChild(contentNode)
        }
        try node.after(try traitSection.outerHtml())
      }
    }

    try document.select(".rusttest, .test-arrow, hr").remove()

    // Remove certain docblock attributes
    for node in try document.select(".docblock.attributes").array() {
      if try node.text().contains("#[must_use]") {
        try node.remove()
      }
    }

    // Handle details elements
    for node in try document.select("details").array() {
      try node.select("summary:contains(Expand description)").remove()
      let children = node.children()
      try node.before(children.outerHtml())
      try node.remove()
    }

    // Fix header links
    for node in try document.select("a.header").array() {
      if let firstChild = node.children().first() {
        let id = try node.attr("name").isEmpty ? node.attr("id") : node.attr("name")
        try firstChild.attr("id", id)
        let children = node.children()
        try node.before(children.outerHtml())
        try node.remove()
      }
    }

    // Normalize heading levels
    for node in try document.select(".docblock > h1:not(.section-header)").array() {
      try node.tagName("h4")
    }
    for node in try document.select("h2.section-header").array() {
      try node.tagName("h3")
    }
    for node in try document.select("h1.section-header").array() {
      try node.tagName("h2")
    }

    // Handle code blocks
    for node in try document.select("pre > code").array() {
      if let classes = try? node.attr("class"), classes.contains("rust") {
        try node.parent()?.attr("data-language", "rust")
      }
      let children = node.children()
      try node.before(children.outerHtml())
      try node.remove()
    }

    for node in try document.select("pre").array() {
      for whereNode in try node.select(".where.fmt-newline").array() {
        try whereNode.before("\n")
      }

      if let classes = try? node.attr("class"), classes.contains("rust") {
        try node.attr("data-language", "rust")
      }
      if try node.hasClass("code-header") {
        try node.attr("data-language", "rust")
      }
    }

    // Set document title for root page
    if context.isRootPage {
      if let h1 = try document.select("h1").first() {
        try h1.text("Rust Documentation")
      }
    }

    // Remove unwanted elements
    try document.select("#copy-path, .sidebar, .collapse-toggle").remove()

    return document
  }
}

// MARK: - Rust Entries Filter

public struct RustEntriesFilter: Filter {
  public func apply(to document: Document, context: FilterContext) throws -> Document {
    // This filter extracts entries and adds them to the context
    // The actual entry extraction happens in the scraper
    return document
  }
}

private func resolveTypeId(_ type: rustType, in crate: RustdocCrate) -> Int? {
  switch type {
  case .resolvedPath(let path):
    return path.id
  default:
    return nil
  }
}

extension Entry {
  func withMembers(_ newMembers: [String]) throws -> Entry {
    return try Entry(
      path: self.path,
      kind: self.kind,
      visibility: self.visibility,
      members: newMembers,
      inputParameters: self.inputParameters,
      outputParameters: self.outputParameters,
      typeParameters: self.typeParameters,
      documentation: self.documentation,
      name: self.name,
      id: self.id
    )
  }
}
