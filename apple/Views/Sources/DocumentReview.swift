extension ViewDocument {
    public func documentCard(forKey key: String) -> Card? {
        guard key.hasPrefix("card:") else { return nil }
        return sections.lazy.flatMap(\.items).compactMap { item in
            if case let .card(card) = item, card.icon == .document { card } else { nil }
        }.first
    }

    public func reviewActions(forKey key: String) -> [Action] {
        let barActions = sections.flatMap(\.items).flatMap { item -> [Action] in
            if case let .actions(bar) = item { bar.actions } else { [] }
        }
        var seen = Set<String>()
        return ((documentCard(forKey: key)?.actions ?? []) + barActions).filter { action in
            guard let id = action.id, !id.isEmpty else { return true }
            return seen.insert(id).inserted
        }
    }
}
