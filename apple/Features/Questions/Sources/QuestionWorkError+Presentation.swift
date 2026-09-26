import AlondraDomain

extension QuestionWorkError {
    public var message: String {
        switch self {
        case .unavailable: "Question service is unavailable"
        case .invalidOption: "That option is no longer available. Review the question and choose again."
        case .explanationRequired: "Explain your choice before sending it."
        case .answerRequired: "Write an answer or choose an option before sending it."
        case .noFeedback: "Add a comment or attachment before sending feedback."
        case .notReviewable: "This report has no reviewable artifact."
        case .menuRequiresOption: "Choose one of this terminal menu's options."
        case .menuDoesNotAcceptAttachments: "Terminal menus do not accept attachments."
        }
    }
}
