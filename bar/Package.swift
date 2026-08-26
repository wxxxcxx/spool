// swift-tools-version: 6.0

import PackageDescription

let package = Package(
    name: "PaneruBar",
    platforms: [.macOS(.v13)],
    products: [
        .executable(name: "PaneruBar", targets: ["PaneruBar"]),
    ],
    targets: [
        .executableTarget(name: "PaneruBar"),
        .testTarget(name: "PaneruBarTests", dependencies: ["PaneruBar"]),
    ],
    swiftLanguageModes: [.v5]
)
