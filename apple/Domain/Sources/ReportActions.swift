import AlondraContracts

extension Report {
    public enum PrimaryAction: Sendable {
        case markRead
        /// It sends an artifact for review: its node waits until David approves it or sends his comments.
        case review
    }
    public func primaryAction() -> PrimaryAction {
        artifacts.isEmpty ? .markRead : .review
    }
}
