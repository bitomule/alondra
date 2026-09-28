import AlondraAPIClient
import AlondraContracts
import AlondraDomain
import AlondraRecovery
import Foundation

extension CloudAppServices {
    public func journal() throws -> DeliveryJournal {
        try checkActive()
        if let deliveryJournal { return deliveryJournal }
        let api = api
        let sync = domainSync
        let journal = DeliveryJournal(scope: scope, macID: macID, read: {
            guard let epoch = await sync.state.projection?.dataEpoch else { throw DeliveryJournalError.obsolete }
            var attempts: [DomainV2.DeliveryAttempt] = []
            var cursor: Int64 = 0
            repeat {
                let page = try await api.domainDeliveryAttempts(after: cursor, epoch: epoch)
                attempts += page.attempts
                guard page.hasMore else { break }
                cursor = page.nextAfter
            } while true
            return attempts
        }, decide: { [weak self] observed, retry in
            guard let self, let epoch = await sync.state.projection?.dataEpoch else { return .obsolete }
            // The attempt's expected version makes a repeated decision a stale one, never a second one.
            let attempt = UUID().uuidString
            switch await self.transport(attempt, { api in
                _ = try await api.resolveDomainDeliveryAttempt(observed.attemptId, epoch: epoch, expectedVersion: observed.version,
                                                               decision: retry ? .retry : .discard, operationID: attempt)
            }) {
            case .confirmed: return .confirmed
            case .unknown: return .unknown
            case .obsolete: return .obsolete
            case .rejected(let message): return .rejected(DeliveryJournalError.rejected(message))
            }
        })
        deliveryJournal = journal
        return journal
    }
}
