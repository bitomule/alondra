// swift-tools-version:6.2

import PackageDescription

let package = Package(
    name: "Alondra",
    platforms: [
        .iOS(.v26),
        .macOS(.v26),
    ],
    dependencies: [
        .package(
            url: "https://github.com/appstefan/HighlightSwift",
            exact: "1.1.0"
        ),
        .package(
            url: "https://github.com/pointfreeco/swift-composable-architecture",
            exact: "1.26.2"
        ),
        .package(
            url: "https://github.com/pointfreeco/swift-snapshot-testing",
            exact: "1.19.6"
        ),
        .package(
            url: "https://github.com/pointfreeco/xctest-dynamic-overlay",
            exact: "1.13.1"
        ),
    ]
)
