import SwiftUI

/// A pull request's changes file by file, the way GitHub shows them: a sticky bar per file that
/// folds it, hunks with old and new numbers, added and removed lines in green and red with what
/// changed inside them marked, and long lines wrapped or scrolled sideways. Binary, generated and
/// very large files come folded and say why. Setting `target` scrolls to that file.
public struct DiffViewer: View {
    let files: [DiffFile]
    let collapsed: Set<String>
    let wrapsLines: Bool
    let truncated: Bool
    let highlights: Bool
    let comments: [String: String]
    @Binding var target: String?
    let toggle: (String) -> Void
    let onLine: ((DiffLine) -> Void)?

    @Environment(\.colorScheme) private var colorScheme
    @State private var highlighted: [String: AttributedString] = [:]
    @State private var width: CGFloat = 0

    public init(files: [DiffFile], collapsed: Set<String>, wrapsLines: Bool, truncated: Bool = false, highlights: Bool = true,
                comments: [String: String] = [:], target: Binding<String?> = .constant(nil),
                toggle: @escaping (String) -> Void, onLine: ((DiffLine) -> Void)? = nil) {
        self.files = files
        self.collapsed = collapsed
        self.wrapsLines = wrapsLines
        self.truncated = truncated
        self.highlights = highlights
        self.comments = comments
        _target = target
        self.toggle = toggle
        self.onLine = onLine
    }

    public var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0, pinnedViews: [.sectionHeaders]) {
                    ForEach(files) { file in
                        Section {
                            fileBody(file)
                        } header: {
                            DiffFileHeader(file: file, comments: file.commentCount(in: comments), collapsed: collapsed.contains(file.id)) {
                                withAnimation(.snappy) { toggle(file.id) }
                            }
                            .clipShape(.rect(topLeadingRadius: 10, bottomLeadingRadius: closed(file) ? 10 : 0,
                                             bottomTrailingRadius: closed(file) ? 10 : 0, topTrailingRadius: 10))
                        }
                        .id(file.id)
                    }
                    if truncated { truncation }
                }
                .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { width = $0 }
                .padding(.horizontal, 10)
                .padding(.vertical, 8)
            }
            .onChange(of: target) { _, id in
                guard let id else { return }
                withAnimation(.smooth) { proxy.scrollTo(id, anchor: .top) }
                target = nil
            }
        }
        .task(id: files.map(\.id) + [colorScheme == .dark ? "dark" : "light"]) {
            guard highlights else { return }
            var rendered: [String: AttributedString] = [:]
            for file in files where !file.isBinary {
                guard !Task.isCancelled else { return }
                rendered.merge(await DiffHighlighter.highlight(file, dark: colorScheme == .dark)) { _, new in new }
            }
            guard !Task.isCancelled else { return }
            highlighted = rendered
        }
    }

    /// Folded by hand: only its bar shows, with nothing under it.
    private func closed(_ file: DiffFile) -> Bool { collapsed.contains(file.id) && FoldReason(file) == nil }

    @ViewBuilder private func fileBody(_ file: DiffFile) -> some View {
        if closed(file) {
            Color.clear.frame(height: 12)
        } else {
            styledBody(file)
        }
    }

    private func styledBody(_ file: DiffFile) -> some View {
        Group {
            if collapsed.contains(file.id), let reason = FoldReason(file) {
                folded(file, reason)
            } else if wrapsLines {
                lines(file)
            } else {
                ScrollView(.horizontal) {
                    lines(file).frame(minWidth: width, alignment: .leading)
                }
                .scrollIndicators(.hidden)
            }
        }
        .background(Palette.card)
        .clipShape(.rect(bottomLeadingRadius: 10, bottomTrailingRadius: 10))
        .overlay {
            UnevenRoundedRectangle(bottomLeadingRadius: 10, bottomTrailingRadius: 10).strokeBorder(Palette.hairline)
        }
        .padding(.bottom, 12)
    }

    private func lines(_ file: DiffFile) -> some View {
        let digits = String(file.lines.compactMap { max($0.old ?? 0, $0.new ?? 0) }.max() ?? 0).count
        var commented: [String: String] = [:]
        for line in file.lines { if let anchor = line.anchor, commented[anchor] == nil { commented[anchor] = line.id } }
        return VStack(alignment: .leading, spacing: 0) {
            ForEach(file.lines) { line in
                row(line, digits: digits, showsComment: line.anchor.map { commented[$0] == line.id } ?? false)
            }
        }
    }

    /// A removed and an added line can share a number and so a comment; it shows under the first.
    @ViewBuilder private func row(_ line: DiffLine, digits: Int, showsComment: Bool) -> some View {
        let comment = showsComment ? line.anchor.flatMap { comments[$0] } : nil
        let content = VStack(alignment: .leading, spacing: 0) {
            DiffCodeLine(line: line, coloured: highlighted[line.id], wraps: wrapsLines, digits: digits)
            if let comment {
                Label(comment, systemImage: "text.bubble.fill")
                    .font(.caption).padding(8)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(Palette.dawn.opacity(0.14))
            }
        }
        if let onLine, line.anchor != nil {
            Button { onLine(line) } label: { content.contentShape(.rect) }
                .buttonStyle(.plain)
        } else {
            content
        }
    }

    private func folded(_ file: DiffFile, _ reason: FoldReason) -> some View {
        HStack(spacing: 8) {
            Image(systemName: reason.symbol).foregroundStyle(.secondary).accessibilityHidden(true)
            reason.text.foregroundStyle(.secondary)
            Spacer(minLength: 0)
            if reason.showable {
                Button {
                    withAnimation(.snappy) { toggle(file.id) }
                } label: {
                    Text("Show", tableName: "AlondraDesign", bundle: #bundle)
                }
                .font(.footnote.weight(.semibold))
                .buttonStyle(.borderless)
            }
        }
        .font(.footnote)
        .padding(.horizontal, 12).padding(.vertical, 10)
    }

    private var truncation: some View {
        Label {
            Text("Part of the diff isn’t shown because it’s too large.", tableName: "AlondraDesign", bundle: #bundle)
        } icon: {
            Image(systemName: "scissors")
        }
        .font(.footnote).foregroundStyle(.secondary)
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Palette.card, in: .rect(cornerRadius: 10, style: .continuous))
    }
}

/// Why a file comes folded, said under its bar.
struct FoldReason {
    let symbol: String
    let text: Text
    let showable: Bool

    init?(_ file: DiffFile) {
        if file.isBinary {
            (symbol, text, showable) = ("doc.zipper", Text("Binary file not shown.", tableName: "AlondraDesign", bundle: #bundle), false)
        } else if !file.hasCode {
            let text = file.status == .renamed
                ? Text("File renamed without changes.", tableName: "AlondraDesign", bundle: #bundle)
                : Text("No changes to show.", tableName: "AlondraDesign", bundle: #bundle)
            (symbol, self.text, showable) = ("doc", text, false)
        } else if file.isGenerated {
            (symbol, text, showable) = ("gearshape", Text("Generated file.", tableName: "AlondraDesign", bundle: #bundle), true)
        } else if file.isLarge {
            let lines = file.additions + file.deletions
            (symbol, text, showable) = ("text.alignleft", Text("Large file: \(lines) changed lines.", tableName: "AlondraDesign", bundle: #bundle), true)
        } else {
            return nil
        }
    }
}
