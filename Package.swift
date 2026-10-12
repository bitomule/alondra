// swift-tools-version:6.4

import PackageDescription

let package = Package(
    name: "Alondra",
    platforms: [
        .iOS(.v26),
        .macOS(.v26),
    ],
    dependencies: [
        .package(url: "https://github.com/apple/swift-collections", exact: "1.6.0"),
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
            url: "https://github.com/pointfreeco/swift-issue-reporting",
            exact: "2.1.0"
        ),
    ]
)
