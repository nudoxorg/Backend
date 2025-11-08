import Foundation
import Logging
import SemVer

public enum OutdatedState: String, CaseIterable {
  case upToDate = "Up-to-date"
  case outdatedMinor = "Outdated minor version"
  case outdatedMajor = "Outdated major version"
}

public protocol Documentation: Instrumentable, Encodable {
  var name: String { get }  // The name you'd expect to see it referred to as (Can just be a deritivitve of the slug)
  var slug: String { get }  // The battle-ready slug for encoding and references
  var version: Version { get }  // The precise version of the doc
  var links: [String: URL] { get }  // Any extraneous links like the source page, or the projects home

  func buildPages() async throws -> [DocumentationPage]
}

private enum CodingKeys: String, CodingKey {
  case slug
  case name
  case links
  case version
}

extension Documentation {
  /// Returns the typical pathing for an index
  public var indexPath: String {
    return "\(slug)/index.json"
  }

  public func encode(to encoder: any Encoder) throws {
    var container = encoder.container(keyedBy: CodingKeys.self)

    try container.encode(slug, forKey: CodingKeys.slug)
    try container.encode(name, forKey: CodingKeys.name)
    try container.encode(links, forKey: CodingKeys.links)
    try container.encode(version, forKey: CodingKeys.version)
  }

  /// Returns the typicalR pathing for the meta files
  public var metaPath: String {
    return "\(slug)/meta.json"
  }

  // Utility methods for network requests
  public func fetchJSON(from urlString: String) async throws -> [String: Any] {
    let request = HTTPRequest(logger: logger)
    let response = try await request.request(urlString)
    guard response.isSuccess else {
      throw NewDocsError.networkError(
        "Failed to fetch \(urlString): \(response.statusCode)")
    }
    return try response.asJSON()
  }

  public func getNPMVersion(package: String, tag: String = "latest") async throws -> String {
    let json = try await fetchJSON(from: "https://registry.npmjs.com/\(package)")
    guard let distTags = json["dist-tags"] as? [String: Any],
      let version = distTags[tag] as? String
    else {
      throw NewDocsError.parsingError(
        "Could not parse npm version for \(package)")
    }
    return version
  }

  public func getLatestGitHubRelease(owner: String, repo: String) async throws -> String {
    let json = try await fetchJSON(
      // Getting the release endpoint
      from: "https://api.github.com/repos/\(owner)/\(repo)/releases/latest")
    guard let tagName = json["tag_name"] as? String else {
      throw NewDocsError.parsingError("Could not parse GitHub release tag")
    }
    return tagName.hasPrefix("v") ? String(tagName.dropFirst()) : tagName
  }
}
