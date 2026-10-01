import AlondraContract
import AlondraFeed
import ComposableArchitecture
import Foundation

/// What a rebuilt screen asks of the app around it. The app opens the screen a link points to and
/// turns the outcome of an action into a toast; the screen never reaches for either itself.
@CasePathable
public enum ScreenDelegate: Equatable, Sendable {
    case open(ViewKey)
    case completed(String)
    case failed(String)
}

public extension ScreenDelegate {
    /// The key a view link opens; links to the web or to the Mac open nothing here.
    static func opening(_ link: AlondraContract.Link?) -> ScreenDelegate? {
        guard case .view(let key)? = link else { return nil }
        return .open(ViewKey(rawValue: key))
    }

    init(_ delegate: ActionFeature.Action.Delegate) {
        switch delegate {
        case .completed(let message): self = .completed(message)
        case .failed(let message): self = .failed(message)
        }
    }
}

extension ActionFeature.State {
    /// The action state of an iPhone screen: the document's epoch is filled in as it loads.
    static func ios() -> Self {
        let preferred = Locale.preferredLanguages.first?.split(separator: "-").first
        return Self(dataEpoch: "", client: .ios, lang: preferred == "es" ? "es" : "en")
    }
}

extension ViewsClient {
    /// A fresh copy of one view, for pull to refresh; the screen's own observation keeps it current after.
    func document(_ key: ViewKey) async throws -> ViewDocument? {
        guard case .document(let document) = try await load(key, nil) else { return nil }
        return document
    }
}

enum WireTimestamp {
    /// Daemon timestamps are RFC 3339 with or without milliseconds, in UTC or with an offset.
    static func date(_ raw: String) -> Date? {
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return fractional.date(from: raw) ?? ISO8601DateFormatter().date(from: raw)
    }
}

extension Trailing {
    var instant: Date? {
        guard case .time(let at, _) = self else { return nil }
        return WireTimestamp.date(at)
    }

    var count: Int? {
        guard case .count(let value) = self else { return nil }
        return Int(value)
    }
}

public extension AlondraContract.Action {
    /// The command a daemon action id names: `cancel` in `cancel:t-1@4`.
    var command: String? {
        guard let id, let separator = id.firstIndex(of: ":") else { return nil }
        return String(id[..<separator])
    }
}
