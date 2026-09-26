import AlondraAPIClient
import AlondraCommands
import AlondraContracts
import AlondraShared
import AlondraTasksFeature
import Foundation

extension CloudAppServices {
    public func taskOperations() -> TaskOperations {
        TaskOperations(withdraw: { request in
            await self.taskCommand(.withdrawRequest(request.id), itemID: request.id) { api, id in
                _ = try await api.send(AlondraEndpoints.withdrawRequest(request.id), operationID: id.uuidString)
            }
        }, cancel: { task in
            await self.taskCommand(.cancelTask(task.id), itemID: task.id) { api, id in
                _ = try await api.send(AlondraEndpoints.cancelTask(task.id), operationID: id.uuidString)
            }
        }, reopen: { task in
            await self.taskCommand(.reopenTask(task.id), itemID: task.id) { api, id in
                _ = try await api.send(AlondraEndpoints.reopenTask(task.id), operationID: id.uuidString)
            }
        }, archiveCompleted: {
            await self.taskCommand(.archiveCompleted, itemID: nil) { api, id in
                _ = try await api.send(AlondraEndpoints.archiveCompletedTasks(ids: nil), operationID: id.uuidString)
            }
        }, details: { task in
            try? await self.taskDecode(TaskInFull.self, endpoint: AlondraEndpoints.task(task.id))
        })
    }

    public func scheduleOperations() -> ScheduleOperations {
        ScheduleOperations(list: {
            try await self.taskDecode(ScheduledTaskList.self, endpoint: AlondraEndpoints.schedules()).schedules
        }, save: { draft, scheduleID in
            await self.taskCommand(.task("schedule:" + (scheduleID ?? "new")), itemID: "schedule:" + (scheduleID ?? "new")) { api, id in
                _ = try await api.send(AlondraEndpoints.saveSchedule(JSONEncoder().encode(Self.normalizedScheduleDraft(draft)), id: scheduleID), operationID: id.uuidString)
            }
        }, run: { scheduleID in
            await self.taskCommand(.task("schedule:" + scheduleID), itemID: "schedule:" + scheduleID) { api, id in
                _ = try await api.send(AlondraEndpoints.runSchedule(scheduleID), operationID: id.uuidString)
            }
        }, delete: { scheduleID in
            await self.taskCommand(.task("schedule:" + scheduleID), itemID: "schedule:" + scheduleID) { api, id in
                _ = try await api.send(AlondraEndpoints.deleteSchedule(scheduleID), operationID: id.uuidString)
            }
        }, projects: {
            try await self.taskDecode(ProjectList.self, endpoint: AlondraEndpoints.projects()).projects.map(\.name)
        }, options: { project in
            try await self.taskDecode(OptionsCatalog.self, endpoint: AlondraEndpoints.options(project: project)).options
        })
    }

    public func taskSnapshots() -> AsyncStream<Overview?> {
        AsyncStream(bufferingPolicy: .bufferingNewest(1)) { continuation in
            let task = Task {
                guard (try? checkActive()) != nil else { continuation.finish(); return }
                let updates = await sync.subscribe()
                for await update in updates {
                    guard !Task.isCancelled, (try? checkActive()) != nil else { break }
                    let decoder = JSONDecoder()
                    decoder.keyDecodingStrategy = .convertFromSnakeCase
                    continuation.yield(update.snapshot.flatMap { try? decoder.decode(Overview.self, from: $0.body) })
                }
                continuation.finish()
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }


    private static func normalizedScheduleDraft(_ draft: ScheduledTaskDraft) -> ScheduledTaskDraft {
        var result = draft
        result.disallowedTools = draft.disallowedTools.map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }.filter { !$0.isEmpty }
        return result
    }

    private func taskDecode<Value: Decodable & Sendable>(_ type: Value.Type, endpoint: Endpoint<Data>) async throws -> Value {
        try checkActive()
        let data = try await api.send(endpoint)
        try checkActive()
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(type, from: data)
    }

    private func taskCommand(_ kind: CommandKind, itemID: String?, operation: @escaping @Sendable (AlondraAPIClient, UUID) async throws -> Void) async -> TaskMutationOutcome {
        switch await submitCommand(kind, itemID: itemID, operation: operation) {
        case .confirmed: return .confirmed
        case .unknown(let command): return .unknown(command.id)
        case .obsolete: return .obsolete
        case .rejected(let error): return .rejected(error.localizedDescription)
        }
    }
}
