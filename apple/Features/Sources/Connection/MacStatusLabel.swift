import AlondraContract
import SwiftUI

/// The path a paired Mac answers on, or why it does not, next to the Mac's name.
public struct MacStatusLabel: View {
    let status: MacStatus

    public init(status: MacStatus) { self.status = status }

    public var body: some View {
        switch status {
        case .paths(let local, let account):
            VStack(alignment: .leading, spacing: 4) {
                if let local { path(local) }
                if let account { path(account) }
            }
        case .route(let route):
            Label { Text(route.title) } icon: { Image(systemName: route.systemImage) }
                .foregroundStyle(.secondary)
                .accessibilityIdentifier("mac.route")
        case .connecting:
            Label("Connecting…", systemImage: "arrow.trianglehead.2.clockwise")
                .foregroundStyle(.secondary)
                .accessibilityIdentifier("mac.connecting")
        case .problem(let problem):
            Label { Text(problem.title) } icon: { Image(systemName: problem.systemImage) }
                .foregroundStyle(.orange)
                .accessibilityIdentifier("mac.problem")
        }
    }
    private func path(_ state: ConnectionState) -> some View {
        Group {
            switch state {
            case .connected(let route):
                Label { Text(route.title) } icon: { Image(systemName: route.systemImage) }.foregroundStyle(.secondary)
            case .connecting:
                Label("Connecting…", systemImage: "arrow.trianglehead.2.clockwise").foregroundStyle(.secondary)
            case .unreachable(let problem):
                Label { Text(problem.title) } icon: { Image(systemName: problem.systemImage) }.foregroundStyle(.orange)
            }
        }
    }

}

public extension MacStatus {
    func accessibilityDescription(locale: Locale) -> String {
        func localized(_ resource: LocalizedStringResource) -> String {
            var resource = resource
            resource.locale = locale
            return String(localized: resource)
        }
        return switch self {
        case .route(let route): localized(route.title)
        case .connecting: localized("Connecting…")
        case .problem(let problem): localized(problem.title)
        case .paths(let local, let account):
            [local, account].compactMap { $0 }.map { Self($0).accessibilityDescription(locale: locale) }.joined(separator: ", ")
        }
    }
}

public extension MacChoice {
    var displayName: String { name ?? String(localized: "Mac \(String(macID.prefix(8)))") }
}
