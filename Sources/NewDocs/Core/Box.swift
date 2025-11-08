import Foundation
import SemVer

public protocol Package {
  var slug: String { get set }  // The true/unqualified name
  var name: String { get set }  // The true/unqualified name
  var lang: Language { get set }  // The language it's related to
  var UUID: Int64 { get }  // The identifier of the package
  var source: URL { get }  // The associated parent/hosting platform, and it's destination within. So a github repository link.

  // These are all async due to possible network requests/file IO
  func get_available_versions() async -> Result<[Version], NewDocsError>  // All of the published versions.
  func flags() async -> Result<[String]?, NewDocsError>  // All of the available flags.
  func description() async -> Result<String?, NewDocsError>  // A provided description as to the purpose of this package
  func dependencies() async -> Result<[Package], NewDocsError>  // Any other packages that this package relies on (none is an empty array)
  func dependents() async -> Result<[Package], NewDocsError>  // Any other packages that rely on this package (none is an empty array)

  // We should only require the slug for initialization. The name is purely cosmetic so not required
  init(slug: String, name: String?, description: String?, source: URL)

  func retrieve(at version: Version, flags: [String]?) async throws -> Documentation  // With the information, get the most relevant, precise doc. It's up to the implementer to create default behavior if applicable for the version specifier. Not every language package has the concept of flags so it can just be passed in as nil. Can throw as this is a signficant resolution step. A lot could go wrong.
}
