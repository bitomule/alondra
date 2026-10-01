"""Compiler flags shared by every Swift target in apple/."""

# Swift 6 language mode with complete data-race checking. Libraries and tests compile with the
# same flags so a Sendable hole cannot hide in either.
STRICT_SWIFT_CONCURRENCY_COPTS = [
    "-swift-version",
    "6",
    "-strict-concurrency=complete",
]
