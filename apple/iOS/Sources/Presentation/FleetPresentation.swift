import AlondraShared
import SwiftUI

struct StateGlyph: View {
    let node: Node

    var body: some View {
        Glyph(symbol: node.glyphSymbol, tint: node.glyphTint, pulse: node.glyphPulses)
    }
}

/// A node's tasks under it, each with its state.
struct NodeTasks: View {
    let tasks: [DomainV2.Task]
    let nodeDisplayName: String

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(tasks, id: \.id) { task in
                HStack(alignment: .top, spacing: 8) {
                    Glyph(task: task, size: .tiny)
                        .padding(.top, 1)
                    (Text(task.fleetRowTitle(nodeDisplayName: nodeDisplayName)).font(.subheadline.weight(.medium)).foregroundStyle(Palette.ink)
                        + Text(verbatim: " · \(task.state.label)").font(.caption.weight(.semibold)).foregroundStyle(task.tint))
                        .lineLimit(2)
                }
            }
        }
    }
}

extension NodeOccupancy {
    /// "3 busy nodes", for VoiceOver.
    func spoken(_ count: Int) -> String {
        switch self {
        case .busy: String(localized: "\(count) busy nodes")
        case .free: String(localized: "\(count) free nodes")
        case .waitingMachine: String(localized: "\(count) nodes waiting for the machine")
        }
    }
}
