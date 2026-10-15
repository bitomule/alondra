import AlondraAttachments
import AlondraContract
import ComposableArchitecture
import Foundation

/// A request that left the composer and has not been taken by the daemon: what was written and
/// chosen, and the attachments it carries, which finish uploading here when they had not yet.
@Reducer
public struct OutgoingRequest: Sendable {
    public init() {}

    @ObservableState
    public struct State: Equatable, Identifiable {
        public let id: String
        public let fields: [String: JSONValue]
        public var attachments: AttachmentFeature.State

        public init(id: String, fields: [String: JSONValue], attachments: AttachmentFeature.State) {
            self.id = id
            self.fields = fields
            self.attachments = attachments
        }
    }

    public enum Action: Sendable {
        case attachments(AttachmentFeature.Action)
    }

    public var body: some ReducerOf<Self> {
        Scope(state: \.attachments, action: \.attachments) { AttachmentFeature() }
    }
}
