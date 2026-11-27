import AlondraContract
import Foundation

public extension StepsDraft.Keyword {
    /// The word a lane or a row carries in the viewer's language.
    var localized: String {
        switch self {
        case .given: String(localized: "Given", table: "AlondraDesign", bundle: #bundle)
        case .when: String(localized: "When", table: "AlondraDesign", bundle: #bundle)
        case .then: String(localized: "Then", table: "AlondraDesign", bundle: #bundle)
        case .and: String(localized: "And", table: "AlondraDesign", bundle: #bundle)
        case .but: String(localized: "But", table: "AlondraDesign", bundle: #bundle)
        }
    }
}

public extension StepsDraft {
    static var examplesLabel: String { String(localized: "Examples", table: "AlondraDesign", bundle: #bundle) }

    /// The steps in the three lanes a sheet draws: `And` and `But` join the lane before them.
    func lanes(labelled known: [SheetLane] = []) -> [SheetLane] {
        var lanes: [SheetLane] = []
        for row in rows {
            let step = SheetStep(text: row.text, table: row.table, doc: row.doc)
            let joins = row.keyword == .and || row.keyword == .but
            let kind = joins ? lanes.last?.kind ?? .given : Self.kind(of: row.keyword)
            if let last = lanes.last, joins || last.kind == kind {
                lanes[lanes.count - 1] = SheetLane(kind: last.kind, label: last.label, steps: last.steps + [step])
            } else {
                let label = known.first { $0.kind == kind }?.label ?? Self.keyword(for: kind).localized
                lanes.append(SheetLane(kind: kind, label: label, steps: [step]))
            }
        }
        return lanes
    }

    private static func kind(of keyword: Keyword) -> SheetLaneKind {
        switch keyword {
        case .given, .and, .but: .given
        case .when: .when
        case .then: .then
        }
    }

    private static func keyword(for kind: SheetLaneKind) -> Keyword {
        switch kind {
        case .given, .other: .given
        case .when: .when
        case .then: .then
        }
    }
}
