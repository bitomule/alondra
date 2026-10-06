import AlondraContract
import SwiftUI

/// The path a paired Mac answers on, or why it does not, next to the Mac's name.
public struct MacStatusLabel: View {
    let status: MacStatus

    public init(status: MacStatus) { self.status = status }

    public var body: some View {
        switch status {
        case .route(let route):
            Label { Text(route.title) } icon: { Image(systemName: route.systemImage) }
                .foregroundStyle(.secondary)
                .accessibilityIdentifier("mac.route")
        case .problem(let problem):
            Label { Text(problem.title) } icon: { Image(systemName: problem.systemImage) }
                .foregroundStyle(.orange)
                .accessibilityIdentifier("mac.problem")
        }
    }
}

public extension MacChoice {
    var displayName: String { name ?? String(localized: "Mac \(String(macID.prefix(8)))") }
}
