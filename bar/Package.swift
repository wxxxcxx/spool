// swift-tools-version: 6.0

import PackageDescription

let package = Package(
    name: "SpoolBar",
    platforms: [.macOS(.v13)],
    products: [
        .executable(name: "SpoolBar", targets: ["SpoolBar"]),
    ],
    targets: [
        .executableTarget(name: "SpoolBar"),
        .testTarget(name: "SpoolBarTests", dependencies: ["SpoolBar"]),
    ],
    swiftLanguageModes: [.v5]
)
