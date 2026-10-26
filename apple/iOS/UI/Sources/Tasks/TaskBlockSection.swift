import AlondraContract
import AlondraDesign
import AlondraFeatures
import SwiftUI

/// One block of the active work ("For you · 2"): its tasks as cards 8 pt apart, or, while they only wait, as two
/// short lines each.
struct TaskBlockSection: View {
    let block: TasksBoard.Block
    let openChains: Set<String>
    let open: (AlondraContract.Link) -> Void
    let toggleChain: (String) -> Void
    let perform: (WireAction) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 6) {
                Text(verbatim: block.title)
                Text(verbatim: "·").foregroundStyle(.tertiary)
                Text(block.count, format: .number).monospacedDigit()
            }
            .font(.subheadline.weight(.semibold))
            .foregroundStyle(.secondary)
            .padding(.horizontal, 4)
            .accessibilityElement(children: .combine)
            .accessibilityAddTraits(.isHeader)
            if block.isCompact {
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(block.requests.flatMap { $0.lines(open: openChains) }) { line in
                        if line.isChain {
                            ChainRow(line: line, isOpen: openChains.contains(line.id)) { toggleChain(line.id) }
                                .padding(.horizontal, 16)
                        } else {
                            CompactTaskLine(line: line, open: open).padding(.leading, line.chain == nil ? 0 : 20)
                        }
                    }
                }
                .background(Palette.card.opacity(0.5), in: .rect(cornerRadius: 22, style: .continuous))
            } else {
                ForEach(block.requests) { request in
                    if request.isLoose {
                        TaskLines(lines: request.lines(open: openChains), openChains: openChains, open: open,
                                  toggleChain: toggleChain)
                    } else {
                        RequestSection(request: request, openChains: openChains, open: open, toggleChain: toggleChain,
                                       perform: perform, inBlock: true)
                    }
                }
            }
        }
        .padding(.horizontal, 16)
        .padding(.top, 20)
    }
}

/// A task's card and, under it, the chain of tasks waiting for it: folded behind "+N waiting for this" and, once
/// opened, one short line each.
struct TaskLines: View {
    let lines: [TasksBoard.Line]
    let openChains: Set<String>
    let open: (AlondraContract.Link) -> Void
    let toggleChain: (String) -> Void

    var body: some View {
        ForEach(lines) { line in
            if line.isChain {
                ChainRow(line: line, isOpen: openChains.contains(line.id)) { toggleChain(line.id) }
            } else if line.chain != nil {
                CompactTaskLine(line: line, open: open).padding(.leading, 20)
            } else if line.isWait {
                WaitsRow(line: line, open: open)
            } else {
                TaskCard(line: line, open: open)
            }
        }
    }
}

/// A task that asks nothing of anyone: its title and, under it in small, its state and what it waits for.
struct CompactTaskLine: View {
    let line: TasksBoard.Line
    let open: (AlondraContract.Link) -> Void

    private var detail: String {
        [line.state, line.activity].compactMap { $0 }.joined(separator: " · ")
    }

    var body: some View {
        Button { if let link = line.link { open(link) } } label: {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(MarkdownBlock.line(line.title))
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(Palette.ink)
                        .lineLimit(2)
                    if !detail.isEmpty {
                        Text(MarkdownBlock.line(detail))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .lineLimit(2)
                    }
                }
                .multilineTextAlignment(.leading)
                Spacer(minLength: 8)
                ForEach(line.marks, id: \.self) { mark in
                    Text(verbatim: mark)
                        .font(.caption2.weight(.semibold))
                        .foregroundStyle(.secondary)
                        .padding(.horizontal, 6)
                        .padding(.vertical, 2)
                        .background(Palette.hairline, in: .capsule)
                        .lineLimit(1)
                }
                if line.waiting > 0 { WaitingBadge(count: line.waiting) }
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 8)
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(.rect)
        }
        .buttonStyle(PressStyle())
        .accessibilityElement(children: .combine)
    }
}
