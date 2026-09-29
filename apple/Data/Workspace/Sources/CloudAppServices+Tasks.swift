import AlondraDomain
import AlondraAPIClient
import AlondraContracts
import Foundation

extension CloudAppServices {
    /// Schedules use the v2 schedule routes. The screens keep their presentation value; projects
    /// and options come from the catalog client contract.
    public func scheduleOperations() -> ScheduleOperations {
        ScheduleOperations(list: {
            try await self.checkActive()
            let list = try await self.api.domainSchedules()
            try await self.checkActive()
            return list.schedules.map(ScheduledTask.init)
        }, save: { draft, scheduleID, attempt in
            guard let body = DomainV2.ScheduleDraft(draft) else { return .rejected(String(localized: "Choose a valid time.")) }
            return await self.transport(attempt) { api in
                if let scheduleID {
                    _ = try await api.updateDomainSchedule(DomainV2.ScheduleId(rawValue: scheduleID), draft: body, operationID: attempt)
                } else {
                    _ = try await api.createDomainSchedule(body, operationID: attempt)
                }
            }
        }, run: { scheduleID, attempt in
            await self.transport(attempt) { api in
                _ = try await api.requestDomainScheduleRun(DomainV2.ScheduleId(rawValue: scheduleID), operationID: attempt)
            }
        }, delete: { scheduleID, attempt in
            await self.transport(attempt) { api in
                try await api.deleteDomainSchedule(DomainV2.ScheduleId(rawValue: scheduleID), operationID: attempt)
            }
        }, projects: {
            try await self.checkActive()
            return try await self.api.domainProjects()
        }, options: { project in
            try await self.checkActive()
            return try await self.api.domainOptions(project: project).options
        })
    }

    /// Routes without a command ID are deduplicated by their transport ID, so a retry reuses
    /// the caller's attempt ID and the Mac returns the first result.
    func transport(_ attempt: String, _ operation: @Sendable (AlondraAPIClient) async throws -> Void) async -> TaskMutationOutcome {
        guard (try? checkActive()) != nil else { return .obsolete }
        do {
            try await operation(api)
            guard (try? checkActive()) != nil else { return .obsolete }
            return .confirmed
        } catch {
            guard (try? checkActive()) != nil else { return .obsolete }
            guard neverApplied(error) else { return .unknown }
            if let error = error as? DomainV2.DomainError { return .rejected(error.message) }
            return .rejected(error.localizedDescription)
        }
    }
}
