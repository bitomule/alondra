import AlondraAPIClient
import AlondraCommands
import AlondraContracts
import AlondraRecovery
import Foundation

extension CloudAppServices {
    public func journal() throws -> DeliveryJournal {
        try checkActive()
        if let deliveryJournal { return deliveryJournal }
        let api = api
        let journal = DeliveryJournal(scope: scope, macID: macID, read: {
            struct Listing: Decodable { let attempts: [DeliveryAttempt] }
            return try JSONDecoder().decode(Listing.self, from: await api.send(AlondraEndpoints.deliveryAttempts())).attempts
        }, decide: { [weak self] observed, retry in
            guard let self else { return .obsolete }
            let decision = DeliveryDecision(observed: observed, retry: retry)
            return await self.submitCommand(.resolveDelivery(decision),
                                            itemID: "delivery:\(observed.id)") { api, operationID in
                let body = try await api.send(AlondraEndpoints.resolveDelivery(observed.id, version: observed.version, retry: retry),
                                              operationID: operationID.uuidString)
                try decision.validate(body)
            }
        })
        deliveryJournal = journal
        return journal
    }
}
