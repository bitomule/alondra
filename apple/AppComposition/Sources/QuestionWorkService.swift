import AlondraAPIClient
import AlondraCommands
import AlondraQuestionsFeature
import Foundation

extension CloudAppServices {
    public func questionWorkService() -> QuestionWorkService {
        QuestionWorkService(submit: { operation in
            let result: CommandResult
            switch operation {
            case .answer(let id, let text, let option, let attachments):
                result = await self.submitCommand(.answer(id), itemID: "q\(id)") { api, operationID in
                    try await api.answer(id, text: text, option: option, attachments: attachments, operationID: operationID.uuidString)
                }
            case .moreContext(let id, let note, let attachments):
                result = await self.submitCommand(.moreContext(id), itemID: "q\(id)") { api, operationID in
                    _ = try await api.send(AlondraEndpoints.moreContext(id, note: note, attachments: attachments), operationID: operationID.uuidString)
                }
            case .reviewReport(let id, let verdict):
                result = await self.submitCommand(.review(id), itemID: "r\(id)") { api, operationID in
                    _ = try await api.send(AlondraEndpoints.review(id, verdict: verdict.rawValue), operationID: operationID.uuidString)
                }
            }
            switch result {
            case .confirmed: return .confirmed
            case .unknown: return .uncertain
            case .obsolete: return .obsolete
            case .rejected(let error):
                switch error as? APIError {
                case .conflict(_, let detail, _): return .conflict(detail ?? "This item changed. Refresh it before trying again.")
                case .forbidden(_, let detail, _): return .forbidden(detail ?? "You do not have permission to perform this action.")
                default: return .failed(error.localizedDescription)
                }
            }
        }, submitReportFeedback: { request, identity in
            await self.submitReportFeedback(request, identity: identity)
        }, document: { path in
            try await self.documentText(path)
        })
    }
}
