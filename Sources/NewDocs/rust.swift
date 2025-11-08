// MARK: - Cargo Package Registry

import Foundation
import Logging
import SemVer
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
          let description = crateData["description"] as? String
        else { return nil }
        return CargoPackage(slug: name, name: name, description: description)
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

      let description = crateData["description"] as? String
      let package = CargoPackage(slug: name, name: name, description: description)
      return .success([package])
    } catch {
      return .failure(.networkError(error.localizedDescription))
    }
  }

  public func get_reference() async -> Package {
    return CargoPackage(
      slug: "rust-reference",
      name: "The Rust Reference",
      description: "The Rust Language Reference"
    )
  }
}

// MARK: - Cargo Package

public struct CargoPackage: Package {
  public var slug: String
  public var name: String
  public var lang: Language = .Rust
  public let UUID: Int64
  public let source: String = "cargo"

  private let packageDescription: String?
  private let httpClient: HTTPRequesting
  private let logger: Logger

  public init(slug: String, name: String?, description: String? = nil) {
    self.slug = slug
    self.name = name ?? slug
    self.packageDescription = description
    self.UUID = Int64(slug.hashValue)
    self.logger = Logger(label: "CargoPackage[\(slug)]")
    self.httpClient = HTTPRequest(logger: logger)
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
        return CargoPackage(slug: name, name: name)
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
          let name = crateData["name"] as? String
        else { return nil }
        return CargoPackage(slug: name, name: name)
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
    let jsonURL = try rustdocJSONURL()
    logger.info("Fetching rustdoc JSON from \(jsonURL)")
    let response = try await httpClient.request(jsonURL)

    // Decompress ZSTD
    let url = URL(filePath: "./example.json")
    let out = URL(filePath: "./out.json")
    let decompressedData = try Data(contentsOf: url)
    let crateData = try JSONDecoder().decode(RustdocCrate.self, from: decompressedData)

    let entries = try mapCrateToEntries(crateData)
    if let jsonData = try? JSONEncoder().encode(entries),
   let jsonString = String(data: jsonData, encoding: .utf8) {
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
      guard let name = item.name else { continue }
      let fqPath = getPath(for: id, name: name)
      let visibility = item.visibility
      let docs = item.docs

      var members: [String]? = nil
      var inputParams: [Parameter]? = nil
      var outputParams: [Parameter]? = nil
      let typeParams: [String]? = nil

      switch item.inner {
      case .module(let module):
        members = module.items.compactMap { crate.index[$0]?.name }
      case .function(let fn):
        inputParams = fn.sig.inputs.map { tuple in
          Parameter(
            name: tuple.name, type: tuple.type.toType(), attributes: nil,
            defaultValue: nil,
            description: nil)
        }
        if let output = fn.sig.output {
          outputParams = [
            Parameter(
              name: "return", type: output.toType(), attributes: nil, defaultValue: nil,
              description: nil)
          ]
        }
      default:
        break
      }

      let entry = try Entry(
        path: fqPath,
        kind: Kind.constant,
        visibility: "hi",
        members: members,
        inputParameters: inputParams,
        outputParameters: outputParams,
        typeParameters: typeParams,
        documentation: docs,
        name: name
      )
      entries[id] = entry
    }

    // Second pass: link associated items
    for (id, item) in crate.index {
      switch item.inner {

      // Link impl items to parent type
      case .impl(let implBlock):
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

      // Link trait associated items
      case .traitItem(let traitData):
        if var traitEntry = entries[id] {
          let assocNames = traitData.items.compactMap { crate.index[$0]?.name }
          var updatedMembers = traitEntry.members ?? []
          updatedMembers.append(contentsOf: assocNames)
          traitEntry = try traitEntry.withMembers(updatedMembers)
          entries[id] = traitEntry
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
      name: self.name
    )
  }
}
