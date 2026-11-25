"""Compiler flags shared by every Swift target in apple/."""

# Swift 6 language mode with complete data-race checking. Libraries and tests compile with the
# same flags so a Sendable hole cannot hide in either.
STRICT_SWIFT_CONCURRENCY_COPTS = [
    "-swift-version",
    "6",
    "-strict-concurrency=complete",
]

# These modules import UIKit or iOS-only SwiftUI API. Declaring it keeps a build of `//apple/...`,
# which compiles for the Mac, from trying them; the iOS apps and test bundles that use them build
# for iOS, where the constraint holds.
IOS_ONLY = ["@platforms//os:ios"]
