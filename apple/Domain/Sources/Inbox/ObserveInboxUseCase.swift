import AlondraContracts
import Foundation

public struct TriageIdentity: Equatable, Sendable {
    public let server: URL
    public let deviceID: String
    public let credentialRevision: String
    public let generation: UInt64

    public init(server: URL, deviceID: String, credentialRevision: String, generation: UInt64) {
        self.server = server
        self.deviceID = deviceID
        self.credentialRevision = credentialRevision
        self.generation = generation
    }
}

public enum TriageConnection: Equatable, Sendable {
    case connecting, online
    case offline(ConnectionProblem)

    public var problem: ConnectionProblem? {
        if case .offline(let problem) = self { return problem }
        return nil
    }
}

public struct TriageInboxUpdate: Sendable {
    public let identity: TriageIdentity
    public let overview: Overview?
    public let connection: TriageConnection

    public init(identity: TriageIdentity, overview: Overview?, connection: TriageConnection = .connecting) {
        self.identity = identity
        self.overview = overview
        self.connection = connection
    }
}

public struct TriageInbox: Equatable, Sendable {
    public let items: [InboxItem]
    public let questionsByPullRequest: [UInt64: [Question]]
    public let reportsByPullRequest: [UInt64: [Report]]
    public let workingNodes: Int
    public let waitingNodes: Int
    public let totalNodes: Int

    public init(
        items: [InboxItem], questionsByPullRequest: [UInt64: [Question]],
        reportsByPullRequest: [UInt64: [Report]], workingNodes: Int,
        waitingNodes: Int, totalNodes: Int
    ) {
        self.items = items
        self.questionsByPullRequest = questionsByPullRequest
        self.reportsByPullRequest = reportsByPullRequest
        self.workingNodes = workingNodes
        self.waitingNodes = waitingNodes
        self.totalNodes = totalNodes
    }
}

/// Converts the server's one overview into a single queue. A question or report attached to an
/// open PR belongs to that PR card; duplicate IDs in a malformed/replayed snapshot are discarded.
/// The reducer retains the ring cursor across updates and does not need to know these rules.
public struct ObserveInboxUseCase: Sendable {
    public init() {}

    public func execute(_ overview: Overview) -> TriageInbox {
        let questions = overview.openQuestions.filter { $0.toOrchestrator != true }
        let reports = overview.news.filter { $0.toOrchestrator != true }
        let prs = overview.prs ?? []
        let grouped = InboxGrouping(questions: questions, reports: reports, pullRequests: prs)
        let candidates = grouped.loose.map(InboxItem.question)
            + prs.map(InboxItem.pullRequest)
            + grouped.looseReports.map(InboxItem.report)
        var seen = Set<String>()
        let unique = candidates.filter { seen.insert($0.id).inserted }
        let nodes = overview.nodes
        return TriageInbox(
            items: InboxRing.ordered(unique),
            questionsByPullRequest: grouped.byPullRequest.mapValues { childQuestions in
                var seen = Set<UInt64>()
                return childQuestions.filter { seen.insert($0.id).inserted }
                    .sorted { $0.askedAt < $1.askedAt }
            },
            reportsByPullRequest: grouped.reportsByPullRequest.mapValues { childReports in
                var seen = Set<UInt64>()
                return childReports.filter { seen.insert($0.id).inserted }
                    .sorted { $0.createdAt < $1.createdAt }
            },
            workingNodes: nodes.count { $0.state == .working },
            waitingNodes: nodes.count {
                $0.state == .waitingAnswer || $0.state == .yourTurn
                    || $0.state == .waitingMerge || $0.state == .waitingMachine
            },
            totalNodes: nodes.count
        )
    }
}
