// swift-tools-version: 6.0
import PackageDescription

let package = Package(
  name: "NewDocs",
  platforms: [
    .macOS(.v15),
    .iOS(.v16),
  ],
  products: [
    .executable(name: "NewDocs", targets: ["NewDocs"])
  ],
  dependencies: [
    .package(url: "https://github.com/scinfu/SwiftSoup.git", from: "2.6.0"),
    .package(url: "https://github.com/facebook/zstd", from: "1.5.6"),
    .package(url: "https://github.com/Alamofire/Alamofire.git", from: "5.8.0"),
    .package(url: "https://github.com/apple/swift-log.git", from: "1.0.0"),
    .package(url: "https://github.com/sersoft-gmbh/semver", from: "5.0.0"),
  ],
  targets: [
    .executableTarget(
      name: "NewDocs",
      dependencies: [
        "SwiftSoup",
        "Alamofire",
        .product(name: "SemVer", package: "semver"),
        .product(name: "libzstd", package: "zstd"),
        .product(name: "Logging", package: "swift-log"),
      ]
    )
  ]
)
