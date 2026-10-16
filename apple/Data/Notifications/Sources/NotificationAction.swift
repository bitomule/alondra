import AlondraContract
import Foundation

/// Resolves a notification button to the opaque action the daemon rendered into the push.
/// No inbox read or action translation is needed on the client.
public struct NotificationAction: Equatable, Sendable {
    public let actionID: String
    public let optionID: String?
    public let text: String?
    public let lang: String

    public init?(item: NotificationPayload.Item, action: String, text: String?, lang: String) {
        guard let index = NotificationPayload.Action.index(action), item.actions.indices.contains(index) else { return nil }
        let offered = item.actions[index]
        if offered.text {
            guard let text = text?.trimmingCharacters(in: .whitespacesAndNewlines), !text.isEmpty else { return nil }
            self.text = text
        } else {
            guard text == nil else { return nil }
            self.text = nil
        }
        actionID = offered.id
        optionID = offered.option
        self.lang = lang == "es" ? "es" : "en"
    }

    public func request(for item: NotificationPayload.Item, commandID: String = UUID().uuidString.lowercased()) -> ActionRequest {
        ActionRequest(commandId: commandID, dataEpoch: item.epoch, client: .ios, lang: lang,
                      actionId: actionID, optionId: optionID, input: ActionInput(text: text))
    }
}

public enum NotificationActionResult: Equatable, Sendable {
    case sent(ActionResponse)
    /// The Mac could not be reached; the answer goes once the app is open.
    case kept
    case uncertain
    case failed(String)
    case noLongerPending
}
