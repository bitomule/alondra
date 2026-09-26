import Foundation

public struct DownloadedAttachment: Equatable, Sendable {
    public let url: URL
    public let name: String
    public init(url: URL, name: String) { self.url = url; self.name = name }
}
