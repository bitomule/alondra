import AlondraContract
import SwiftUI

/// The way out a screen showing saved data offers: try again now, or open Local Network settings.
public struct ConnectionRecovery {
    public let retry: () -> Void
    public let localNetworkSettings: (() -> Void)?

    public init(retry: @escaping () -> Void, localNetworkSettings: (() -> Void)?) {
        self.retry = retry
        self.localNetworkSettings = localNetworkSettings
    }
}

public extension EnvironmentValues {
    @Entry var connectionRecovery: ConnectionRecovery?
}

/// «saved data · <why>» over a snapshot the Mac is not refreshing. Tapping it shows the whole
/// problem and what to do; the app keeps retrying on its own meanwhile.
public struct SavedDataLabel: View {
    let problem: ConnectionProblem
    @Environment(\.connectionRecovery) private var recovery
    @State private var showsProblem = false

    public init(problem: ConnectionProblem) { self.problem = problem }

    private var label: some View {
        Label {
            Text("saved data · \(Text(problem.summary))")
        } icon: {
            Image(systemName: problem.systemImage)
        }
    }

    public var body: some View {
        Group {
            if recovery == nil {
                label
            } else {
                Button { showsProblem = true } label: { label }
                    .buttonStyle(.plain)
            }
        }
        .accessibilityIdentifier("connection.savedData")
        .sheet(isPresented: $showsProblem) {
            if let recovery {
                ConnectionLostView(problem: problem, retry: {
                    showsProblem = false
                    recovery.retry()
                }, localNetworkSettings: recovery.localNetworkSettings)
                .presentationDetents([.medium])
            }
        }
    }
}

/// The line under a screen's title: its own summary while the Mac answers, otherwise what the
/// connection state says about the saved data on screen.
public struct ConnectionLine<Summary: View>: View {
    let line: ConnectionIndicators.Line
    let alert: AnyShapeStyle
    let summary: Summary

    public init(_ line: ConnectionIndicators.Line, alert: some ShapeStyle = .orange, @ViewBuilder summary: () -> Summary) {
        self.line = line
        self.alert = AnyShapeStyle(alert)
        self.summary = summary()
    }

    public var body: some View {
        switch line {
        case .summary:
            summary
        case .connecting:
            Label("Connecting…", systemImage: "arrow.trianglehead.2.clockwise")
                .foregroundStyle(.secondary)
                .accessibilityIdentifier("connection.connecting")
        case .savedData(let problem):
            SavedDataLabel(problem: problem).foregroundStyle(alert)
        }
    }
}
