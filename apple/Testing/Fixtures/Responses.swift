import AlondraContract
import AlondraFeed
import ComposableArchitecture
import Foundation

enum Responses {
    /// The daemon's answer to an action that went through; `message` is what the toast says.
    static func applied(_ message: String = "Saved") throws -> ActionResponse {
        try response(#"{"protocol":1,"outcome":"applied","stamp":"\#(message)","message":"\#(message)","revision":2}"#)
    }

    static func rejected(_ message: String, field: String? = nil) throws -> ActionResponse {
        try response(#"{"protocol":1,"outcome":"rejected","message":"\#(message)"\#(field.map { #","field":"\#($0)""# } ?? "")}"#)
    }

    /// A refusal as the server words it now: the typed `result` carries the sentence the card shows.
    static func refused(_ title: String) throws -> ActionResponse {
        try response(#"{"protocol":1,"outcome":"rejected","message":"raw domain text","result":{"kind":"refused","title":"\#(title)"}}"#)
    }

    private static func response(_ json: String) throws -> ActionResponse {
        try ContractCodec.decode(ActionResponse.self, from: Data(json.utf8))
    }
}

extension ViewsClient {
    /// The view arrives once and the observation then ends, so a test's store can finish.
    static func showing(_ document: ViewDocument) -> @Sendable (ViewKey) -> AsyncThrowingStream<ViewFetch, any Error> {
        { _ in AsyncThrowingStream { $0.yield(.document(document)); $0.finish() } }
    }
}

/// The requests a screen sent to the daemon, in order.
final class SentActions: @unchecked Sendable {
    private let lock = NSLock()
    private var sent: [ActionRequest] = []

    var requests: [ActionRequest] { lock.withLock { sent } }

    func record(_ request: ActionRequest) { lock.withLock { sent.append(request) } }
}
