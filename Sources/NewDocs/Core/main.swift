import Foundation
import Logging
import SemVer

struct GenerateRustReference {
	static func main() async {
		do {
			// 1) Create the NewDocumentations driver
			let newdocs = NewDocumentations()

			// 2) Get the Rust registry and the reference package
			let rustRegistry = newdocs.registry(for: .Rust)

			let options = await rustRegistry.search_packages(for: "serde")

			let theThing = try options.get()

			let version = Version(major: 1, minor: 1, patch: 0)

			let doc = try await newdocs.buildPackage(
				package: theThing[0],
				version: version
			)

			// 5) Prepare a local file‐system store
			let store = try FileSystemStore(baseDirectory: "./output")

			// 6) Persist all pages, index.json, db.json, meta.json
			try await DocumentationStorer().store(doc, to: store)

			// 7) Finally, write docs.json manifest
			try await Manifest().generate(docs: [doc], store: store)

			print("✅ Rust reference generated at ./output/\(doc.slug)/")
		} catch {
			print("❌ Failed to generate Rust reference:", error)
			exit(1)
		}
	}
}

await GenerateRustReference.main()
