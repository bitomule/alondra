import ComposableArchitecture

/// Gets a command that signs in to Claude ready in Terminal on the daemon's Mac. Only the Mac app
/// on its own daemon can: it copies the command and opens Terminal. Everywhere else it answers
/// false and the person runs the command on the Mac.
@DependencyClient
public struct ClaudeTerminalClient: Sendable {
    public var open: @Sendable (_ command: String) async -> Bool = { _ in false }
}

extension ClaudeTerminalClient: DependencyKey {
    public static let liveValue = ClaudeTerminalClient(open: { _ in false })
    public static let testValue = ClaudeTerminalClient()
}

public extension DependencyValues {
    var claudeTerminal: ClaudeTerminalClient {
        get { self[ClaudeTerminalClient.self] }
        set { self[ClaudeTerminalClient.self] = newValue }
    }
}
