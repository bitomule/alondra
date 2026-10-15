import SwiftUI

public extension PendingRequests.Phase {
    /// What the queue says about a request this device sent, the same on iPhone and Mac.
    var label: LocalizedStringKey {
        switch self {
        case .waiting(.options): "Waiting to send"
        case .waiting(.attachments): "Uploading attachments…"
        case .sending: "Sending…"
        case .unconfirmed: "Not confirmed yet · Retrying…"
        case .failed: "Not sent"
        case .applied: "Queued"
        }
    }

    /// The symbol beside the label: how the request is going at a glance.
    var symbol: String {
        switch self {
        case .waiting(.options): "clock"
        case .waiting(.attachments): "arrow.up.doc"
        case .sending: "paperplane"
        case .unconfirmed: "arrow.triangle.2.circlepath"
        case .failed: "exclamationmark.circle"
        case .applied: "tray"
        }
    }

    var failure: String? { if case .failed(let message) = self { message } else { nil } }
}
