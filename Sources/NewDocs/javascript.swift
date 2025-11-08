import Foundation
import Logging
import SemVer
import SwiftSoup
import zlib

public struct NPMRegistry: PackageRegistry {
  private let logger: Logger
  private let httpClient: HTTPRequesting

  public init(logger: Logger = Logger(label: "NPMRegistry")) {
    self.logger = logger
    self.httpClient = HTTPRequest(logger: logger)
  }

  public func search_packages(for query: String) async -> Result<[Package], NewDocsError> {
    do {
      let encodedQuery =
        query.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? query
      let response = try await httpClient.request(
        "https://registry.npmjs.org/-/v1/search?text=\(encodedQuery)")
      guard response.isSuccess else {
        return .failure(
          .networkError("Failed to search npm registry: \(response.statusCode)"))
      }
      let json = try response.asJSON()
      guard let objects = json["objects"] as? [[String: Any]] else {
        return .failure(.parsingError("Invalid npm search response format"))
      }
      let packages = objects.compactMap { obj -> Package? in
        guard let pkg = obj["package"] as? [String: Any],
          let name = pkg["name"] as? String
        else {
          return nil
        }
        let description = pkg["description"] as? String
        let links = pkg["links"] as? [String: Any]
        let repository = links?["repository"] as? String
        let sourceURL =
          repository.flatMap { URL(string: $0) }
          ?? URL(string: "https://www.npmjs.com/package/\(name)")!
        return NPMPackage(slug: name, name: name, description: description, source: sourceURL)
      }
      return .success(packages)
    } catch {
      return .failure(.networkError(error.localizedDescription))
    }
  }

  public func get_package(UUID: UInt64) async -> Result<Package, NewDocsError> {
    return .failure(.invalidEntry("UUID-based lookup not supported for NPM packages"))
  }

  public func get_package(named: String) async -> Result<[Package], NewDocsError> {
    do {
      let response = try await httpClient.request(
        "https://registry.npmjs.org/\(named)")
      guard response.isSuccess else {
        return .failure(
          .networkError("Failed to fetch npm packages: \(response.statusCode)"))
      }
      let json = try response.asJSON()
      guard let name = json["name"] as? String else {
        return .failure(.parsingError("Invalid npm package response format"))
      }
      let description = json["description"] as? String
      let repository = (json["repository"] as? [String: Any])?["url"] as? String
      let sourceURL =
        repository.flatMap { URL(string: $0) }
        ?? URL(string: "https://www.npmjs.com/package/\(name)")!
      return .success([
        NPMPackage(slug: name, name: name, description: description, source: sourceURL)
      ])
    } catch {
      return .failure(.networkError(error.localizedDescription))
    }
  }

  public func get_reference() async -> Package {
    NPMPackage(
      slug: "javascript-reference",
      name: "MDN JavaScript Reference",
      description: "Authoritative JavaScript reference from MDN Web Docs",
      source: URL(string: "https://developer.mozilla.org/en-US/docs/Web/JavaScript")!)
  }
}

public struct NPMPackage: Package {
  public var slug: String
  public var name: String
  public var lang: Language = .Javascript
  public let UUID: Int64
  public var source: URL
  private let packageDescription: String?
  private let httpClient: HTTPRequesting
  private let logger: Logger

  public init(slug: String, name: String?, description: String?, source: URL) {
    self.slug = slug
    self.name = name ?? slug
    self.packageDescription = description
    self.source = source
    self.UUID = Int64(slug.hashValue)
    self.logger = Logger(label: "NPMPackage[\(slug)]")
    self.httpClient = HTTPRequest(logger: logger)
  }

  public func get_available_versions() async -> Result<[Version], NewDocsError> {
    do {
      let encodedSlug =
        slug.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? slug
      let response = try await httpClient.request("https://registry.npmjs.org/\(encodedSlug)")
      guard response.isSuccess else {
        return .failure(.networkError("Failed to fetch versions: \(response.statusCode)"))
      }
      let json = try response.asJSON()
      guard let versions = json["versions"] as? [String: Any] else {
        return .failure(.parsingError("Invalid npm versions response"))
      }
      let semverVersions = versions.keys.compactMap { try? Version($0) }.sorted(by: >)
      return .success(semverVersions)
    } catch {
      return .failure(.networkError(error.localizedDescription))
    }
  }

  public func flags() async -> Result<[String]?, NewDocsError> {
    return .success(nil)  // npm packages don't have global feature flags
  }

  public func description() async -> Result<String?, NewDocsError> {
    return .success(packageDescription)
  }

  public func dependencies() async -> Result<[Package], NewDocsError> {
    do {
      let encodedSlug =
        slug.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? slug
      let response = try await httpClient.request("https://registry.npmjs.org/\(encodedSlug)")
      guard response.isSuccess else {
        return .failure(
          .networkError("Failed to fetch dependencies: \(response.statusCode)"))
      }
      let json = try response.asJSON()
      guard
        let distTags = json["dist-tags"] as? [String: Any],
        let latestTag = distTags["latest"] as? String,
        let versions = json["versions"] as? [String: Any],
        let latest = versions[latestTag] as? [String: Any]
      else {
        return .failure(.parsingError("Invalid npm dependency response"))
      }
      guard let deps = latest["dependencies"] as? [String: Any] else {
        return .success([])
      }
      let packages = deps.keys.map {
        NPMPackage(
          slug: $0,
          name: $0,
          description: nil,
          source: URL(string: "https://www.npmjs.com/package/\($0)")!
        )
      }
      return .success(packages)
    } catch {
      return .failure(.networkError(error.localizedDescription))
    }
  }

  public func dependents() async -> Result<[Package], NewDocsError> {
    // npm doesn't expose reverse dependency data via a stable API
    return .success([])
  }

  public func retrieve(at version: Version, flags: [String]?) async throws -> Documentation {
    return JavaScriptDocScraper(package: self, version: version)
  }
}

public struct JavaScriptDocScraper: Documentation {
  public let logger: Logger
  public let package: NPMPackage
  public let version: Version
  private let httpClient: HTTPRequesting

  public var name: String { package.name }
  public var slug: String { package.slug }
  public var links: [String: URL] {
    return [
      "home": URL(string: "https://www.npmjs.com/package/\(package.slug)")!,
      "docs": URL(string: "https://unpkg.com/\(package.slug)@\(version)/")!,
      "registry": URL(string: "https://registry.npmjs.org/\(package.slug)")!,
    ]
  }

  public init(package: NPMPackage, version: Version) {
    self.package = package
    self.version = version
    self.logger = Logger(label: "JavaScriptDocScraper[\(package.slug)]")
    self.httpClient = HTTPRequest(logger: logger)
  }

  /// This function builds pages by scraping TypeScript definitions and README
  public func buildPages() async throws -> [DocumentationPage] {
    // Decompress ZSTD
    let out = URL(filePath: "./out.json")

    let bin = Process()
    bin.executableURL =
      URL(
        fileURLWithPath:
          "/etc/profiles/per-user/philocalyst/bin/npx")

    let arguments = [
      "build",
      "index.js",
      "-f",
      "json",
    ]

    bin.arguments = arguments

    let json = try await getJSON(
      source: self.package.source, command: bin, output_location: nil)

    print(json)

    let page = DocumentationPage(path: [], internalURLs: [], entries: [])
    return [page]
  }
}
