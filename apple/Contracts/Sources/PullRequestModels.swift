import Foundation

public struct PRCheck: Codable, Hashable, Sendable {
    public let name: String
    public let state: String
    public let url: String?

    public init(name: String, state: String, url: String? = nil) {
        self.name = name
        self.state = state
        self.url = url
    }

    public var passed: Bool { state == "passed" }
    public var failed: Bool { state == "failed" }
    public var running: Bool { state == "running" }
}
