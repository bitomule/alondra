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
    ]
)
