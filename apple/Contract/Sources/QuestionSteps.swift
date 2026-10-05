import Foundation

/// What the user chose for one question of a card that asks several: the option's id, or their own words.
public struct QuestionAnswer: Equatable, Sendable {
    public let optionID: String?
    public let text: String?

    public init(optionID: String?, text: String?) {
        self.optionID = optionID
        self.text = text
    }

    var json: JSONValue {
        .object(["option_id": optionID.map(JSONValue.string) ?? .null, "text": text.map(JSONValue.string) ?? .null])
    }
}

/// How far the user is through a card that asks several questions. The card shows one question at
/// a time; nothing is sent until the last one is answered, and then every answer goes together.
public struct QuestionSteps: Equatable, Sendable {
    public private(set) var answers: [QuestionAnswer] = []

    public init() {}

    /// The question shown: the first still unanswered.
    public func index(in card: Card) -> Int { min(answers.count, max(card.steps.count - 1, 0)) }

    public func shown(in card: Card) -> CardStep? {
        card.steps.isEmpty ? nil : card.steps[index(in: card)]
    }

    public var canGoBack: Bool { !answers.isEmpty }

    /// The question's buttons, as many as the card has room for; what does not fit goes under "More…",
    /// before the way to answer in words.
    public func buttons(in card: Card, limit: Int) -> [Action] {
        shown(in: card).map { Array($0.actions.prefix(limit)) } ?? []
    }

    public func menu(in card: Card, limit: Int) -> [Action] {
        guard let step = shown(in: card) else { return [] }
        return Array(step.actions.dropFirst(limit)) + (step.write.map { [$0] } ?? [])
    }

    /// Records the choice for the question shown. The fields to send come back once the last
    /// question has its answer; before that the card moves on to the next question.
    public mutating func choose(_ action: Action, text: String? = nil, in card: Card) -> [String: JSONValue]? {
        guard let step = shown(in: card), answers.count < card.steps.count else { return nil }
        guard step.actions.contains(action) || step.write == action else { return nil }
        let words = text?.trimmingCharacters(in: .whitespacesAndNewlines).nilIfEmpty
        guard action.optionId != nil || words != nil else { return nil }
        answers.append(QuestionAnswer(optionID: action.optionId, text: words))
        guard answers.count == card.steps.count else { return nil }
        return ["answers": .array(answers.map(\.json))]
    }

    public mutating func back() {
        _ = answers.popLast()
    }
}

private extension String {
    var nilIfEmpty: String? { isEmpty ? nil : self }
}
