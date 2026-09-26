import Foundation

public enum TriageMutationOutcome: Equatable, Sendable {
    case confirmed
    case rejected(String)
    case unknown
    case unknownCommand(UUID)
    case obsolete
}

public struct TriageService: Sendable {
    public var observe: @Sendable () async -> AsyncStream<TriageInboxUpdate>
    public var mutate: @Sendable (CommandKind, String, TriageIdentity) async -> TriageMutationOutcome

    public init(
        observe: @escaping @Sendable () async -> AsyncStream<TriageInboxUpdate>,
        mutate: @escaping @Sendable (CommandKind, String, TriageIdentity) async -> TriageMutationOutcome
    ) {
        self.observe = observe
        self.mutate = mutate
    }
}
