import Foundation

/// The original send-failure toast wording, shared by presentation owners.
public func couldNotSendMessage(_ detail: String) -> String {
    String(localized: "Couldn’t send: \(detail)", table: "AlondraDesign", bundle: #bundle)
}
