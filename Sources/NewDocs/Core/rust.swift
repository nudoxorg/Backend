// MARK: - Cargo Package Registry

import Foundation
import Logging
import SemVer
import SwiftSoup

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
				return .failure(
					.networkError("Failed to fetch dependencies: \(response.statusCode)"))
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

public struct RustDocScraper: Scraper {
	public let logger: Logger
	public let baseURL: URL
	public let rootURL: URL
	public let rootPath: String?
	public let initialPaths: [URL]
	public let options: ScraperOptions
	public let source: ScraperSource
	public var htmlFilters: FilterStack
	public var textFilters: FilterStack

	private let package: CargoPackage
	public let version: Version
	private let features: [String]
	private let httpClient: HTTPRequesting
	private let rateLimiter: RateLimiter?

	public var name: String {
		return package.name
	}

	public var slug: String {
		return package.slug
	}

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
		return package.slug == "std" || package.slug == "rust-reference"
	}

	public init(package: CargoPackage, version: Version, features: [String] = []) {
		self.package = package
		self.version = version
		self.features = features
		self.logger = Logger(label: "RustDocScraper[\(package.slug)]")
		self.httpClient = HTTPRequest(logger: logger)

		// Configure URLs based on package type
		if package.slug == "rust-reference" {
			self.baseURL = URL(string: "https://doc.rust-lang.org/")!
			self.rootURL = URL(string: "https://doc.rust-lang.org/reference/introduction.html")!
			self.rootPath = "reference/introduction.html"
			self.initialPaths = []
		} else if package.slug == "std" {
			self.baseURL = URL(string: "https://doc.rust-lang.org/")!
			self.rootURL = URL(string: "https://doc.rust-lang.org/book/index.html")!
			self.rootPath = "book/index.html"
			self.initialPaths = [
				URL(string: "https://doc.rust-lang.org/reference/introduction.html")!,
				URL(string: "https://doc.rust-lang.org/std/index.html")!,
				URL(string: "https://doc.rust-lang.org/error-index.html")!,
			]
		} else {
			self.baseURL = URL(string: "https://docs.rs/\(package.slug)/\(version)/")!
			self.rootURL = URL(
				string: "https://docs.rs/\(package.slug)/\(version)/\(package.slug)/")!
			self.rootPath = nil
			self.initialPaths = []
		}

		// Configure scraper options
		self.options = ScraperOptions()
			.withRateLimit(10)
			.withTimeout(30)
			.withRetryCount(3)

		self.source = .remote(rateLimit: 10)
		self.rateLimiter = RateLimiter(limit: 10)

		// Setup filters
		var htmlStack = FilterStack()
		htmlStack.push(RustCleanHtmlFilter())
		self.htmlFilters = htmlStack

		var textStack = FilterStack()
		textStack.push(RustEntriesFilter())
		self.textFilters = textStack
	}

	public func fetch(_ url: URL) async throws -> HTTPResponse {
		await rateLimiter?.waitIfNeeded()

		var fixedURL = url.absoluteString

		// Apply URL fixes similar to Ruby version
		if fixedURL.hasSuffix("/") && !fixedURL.hasSuffix(".html") {
			fixedURL += "index.html"
		}

		fixedURL = fixedURL.replacingOccurrences(of: "/nightly/", with: "/")
		fixedURL = fixedURL.replacingOccurrences(of: "/unicode/u_str", with: "/unicode/str/")
		fixedURL = fixedURL.replacingOccurrences(of: "/std/std/", with: "/std/")

		return try await httpClient.request(fixedURL)
	}

	public func preprocessResponse(_ response: HTTPResponse) -> HTTPResponse {
		// Fix code headers (similar to Ruby's parse hook)
		let fixedBody = response.body.replacingOccurrences(
			of: #"<h[1-6] class="code-header">"#,
			with: #"<pre class="code-header">"#,
			options: .regularExpression
		)

		return HTTPResponse(
			url: response.url,
			statusCode: response.statusCode,
			headers: response.headers,
			data: fixedBody.data(using: .utf8) ?? response.data
		)
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

extension RustDocScraper {
	public func extractEntries(from document: Document, context: FilterContext) throws -> [Entry] {
		var entries: [Entry] = []
		let subpath = context.subpath

		if subpath.hasPrefix("book/") || subpath.hasPrefix("reference/") {
			// Handle book/reference entries
			let name = getName(from: document, subpath: subpath)
			let type = getType(from: document, subpath: subpath)

			if let entry = try? Entry(name: name, path: context.slug, type: type) {
				entries.append(entry)
			}

		} else if subpath == "error-index" {
			// Handle error index
			if let entry = try? Entry(
				name: "Compiler Errors", path: context.slug, type: "Compiler Errors")
			{
				entries.append(entry)
			}

			for node in try document.select(".error-described h2.section-header").array() {
				let content = try node.text()
				guard !content.contains("Note:") else { continue }

				let id = try node.attr("id")
				if let entry = try? Entry(
					name: content, path: "\(context.slug)#\(id)", type: "Error")
				{
					entries.append(entry)
				}
			}

		} else {
			// Handle standard rustdoc
			let name = getName(from: document, subpath: subpath)
			let type = getType(from: document, subpath: subpath)

			if let entry = try? Entry(name: name, path: context.slug, type: type) {
				entries.append(entry)
			}

			// Extract method entries
			for node in try document.select(".method").array() {
				if let fnLink = try node.select("a.fn").first() {
					let methodName = try fnLink.text()
					let fullName = "\(name)::\(methodName)"
					let id = try node.attr("id")

					if let entry = try? Entry(
						name: fullName, path: "\(context.slug)#\(id)", type: "Method")
					{
						entries.append(entry)
					}
				}
			}
		}

		return entries
	}

	private func getName(from document: Document, subpath: String) -> String {
		do {
			if subpath.hasPrefix("book/") || subpath.hasPrefix("reference/") {
				if let heading = try document.select("h2, h1").first() {
					let name = try heading.text()

					// Extract chapter numbers for book
					if let chMatch = subpath.range(
						of: #"ch(\d+)-(\d+)"#, options: .regularExpression)
					{
						let chapterPart = String(subpath[chMatch])
						let components = chapterPart.components(
							separatedBy: CharacterSet(charactersIn: "ch-"))
						if components.count >= 3 {
							return "\(components[1]).\(components[2]). \(name)"
						}
					}

					return name.isEmpty ? "Introduction" : name
				}
				return "Introduction"

			} else if subpath == "error-index" {
				return "Compiler Errors"

			} else {
				if let h1 = try document.select("main h1").first() {
					try h1.select("button").remove()
					var name = try h1.text()
					name = name.replacingOccurrences(
						of: #"^\S+\s"#, with: "", options: .regularExpression)
					name = name.replacingOccurrences(of: "⎘", with: "")

					let mod = String(subpath.split(separator: "/").first ?? "")
					if !name.hasPrefix(mod) && !mod.isEmpty {
						name = "\(mod)::\(name)"
					}

					return name
				}
			}
		} catch {
			logger.error("Error extracting name: \(error)")
		}

		return "Unknown"
	}

	private func getType(from document: Document, subpath: String) -> String {
		do {
			if subpath.hasPrefix("book/") {
				return "Guide"
			} else if subpath.hasPrefix("reference/") {
				return "Reference"
			} else if subpath == "error-index" {
				return "Compiler Errors"
			} else {
				let name = getName(from: document, subpath: subpath)
				let path = name.components(separatedBy: "::")

				if let h1 = try document.select("main h1").first() {
					let heading = try h1.text().trimmingCharacters(in: .whitespacesAndNewlines)

					if path.count > 2
						|| (path.count == 2
							&& (heading.hasPrefix("Module") || heading.hasPrefix("Primitive")))
					{
						return path.prefix(2).joined(separator: "::")
					} else {
						return path.first ?? "Unknown"
					}
				}

				return path.first ?? "Unknown"
			}
		} catch {
			logger.error("Error extracting type: \(error)")
		}

		return "Unknown"
	}
}
