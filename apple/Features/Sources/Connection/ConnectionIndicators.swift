import AlondraContract
import SwiftUI

/// Everything a screen says about the connection, derived together from the one connection state
/// and whether the screen has saved data to show, so no two of them can disagree.
public struct ConnectionIndicators: Equatable, Sendable {
    /// The dot next to the Mac's figures.
    public enum Light: Equatable, Sendable { case live, connecting, down }

    /// The line under a screen's title while it shows saved data.
    public enum Line: Equatable, Sendable {
        /// The screen's own summary: its data is live.
        case summary
        case connecting
        case savedData(ConnectionProblem)
    }

    /// What a screen with nothing to show puts in its place.
    public enum Cover: Equatable, Sendable {
        /// The screen's own loading or failure: the Mac is answering.
        case none
        case connecting
        case lost(ConnectionProblem)
    }

    public let light: Light
    public let line: Line
    public let cover: Cover

    public init(_ state: ConnectionState, hasSavedData: Bool) {
        switch state {
        case .connected:
            light = .live
            line = .summary
            cover = .none
        case .connecting:
            light = .connecting
            line = hasSavedData ? .connecting : .summary
            cover = hasSavedData ? .none : .connecting
        case .unreachable(let problem):
            light = .down
            line = hasSavedData ? .savedData(problem) : .summary
            cover = hasSavedData ? .none : .lost(problem)
        }
    }

    /// The Mac's figures are shown as current only while it answers.
    public var figuresLive: Bool { light == .live }

    /// The line as plain text, for a window's subtitle: `summary` while the data is live.
    public func subtitle(summary: String) -> String {
        switch line {
        case .summary: summary
        case .connecting: String(localized: "Connecting…")
        case .savedData(let problem): String(localized: "saved data · \(String(localized: problem.summary))")
        }
    }
}

public extension EnvironmentValues {
    /// Set once, at the root of each scene, from the app's store. Screens read it; none decides it.
    @Entry var connectionState: ConnectionState = .connected(.relay)
}
