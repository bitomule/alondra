import Foundation

public struct RelayReceipt: Sendable {
    public let requestID: String
    public let status: String
    public let revision: UInt64
    public let response: Envelope?

    public init(requestID: String, status: String, revision: UInt64, response: Envelope?) {
        self.requestID = requestID
        self.status = status
        self.revision = revision
        self.response = response
    }
}

public protocol RelayRequestTransport: Sendable {
    func submitRequest(_ envelope: Envelope) async throws -> RelayReceipt
    func requestStatus(requestID: String) async throws -> RelayReceipt
}
