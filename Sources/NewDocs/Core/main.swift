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

      try await doc.buildPages()

      // 5) Prepare a local file‐system store
      let store = try FileSystemStore(baseDirectory: "./output")

      // 7) Finally, write docs.json manifest
      try await Manifest().generate(docs: [doc], store: store)

      print("✅ Rust reference generated at ./output/\(doc.slug)/")
    } catch {
      print("❌ Failed to generate Rust reference:", error)
      exit(1)
    }
  }
}

struct GenerateJavascriptReference {
	static func main() async {
		do {
			let newdocs = NewDocumentations()

			//search npm registry for express framework
			let npmRegistry = newdocs.registry(for: .Javascript)
			let searchResult = await npmRegistry.search_packages(for: "express")
			let packages = try searchResult.get()
			print("🔎 Found \(packages.count) packages; first:", packages.first ?? "none")

			// pick the first package
			guard let package = packages.first else { return }
			//get latest version
			let version = Version(major: 5, minor: 1, patch: 0)

			/*
			print("NPM package: \(package)")
			print("version: \(version)")
			*/

			let doc = try await newdocs.buildPackage(package: package, version: version)
			_ = try await doc.buildPages()
			let store = try FileSystemStore(baseDirectory: "./output")
			try await Manifest().generate(docs: [doc], store: store)
			print("✅ npm package docs generated at ./output/\(doc.slug)/")
		} catch {
			print("❌ Failed to generate npm docs:", error)
			exit(1)
		}
	}
}

//await GenerateRustReference.main()
await GenerateJavascriptReference.main()
