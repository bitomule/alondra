import AlondraShared
import SwiftUI

struct ReviewView: View {
    let item: InboxItem

    @Environment(FleetStore.self) private var fleet
    @Environment(\.dismiss) private var dismiss

    @State private var blocks: [MarkdownBlock] = []
    @State private var loadError: String?
    @State private var loaded = false
    @State private var comments: [String: String] = [:]
    @State private var general = ""
    @State private var composing: MarkdownBlock?
    @State private var sending = false
    @State private var closing: CloseRequest?
    @FocusState private var generalFocused: Bool

    private var kind: Kind { Kind(item) }

    private var commentedBlocks: [MarkdownBlock] {
        blocks.filter { comments[$0.draftKey] != nil }
    }

    private var feedbackComments: [ReviewComment] {
        commentedBlocks.map { ReviewComment(anchor: $0.draftKey, quote: $0.plainText, text: comments[$0.draftKey] ?? "") }
    }

    private func number(of block: MarkdownBlock) -> Int {
        (commentedBlocks.firstIndex(of: block) ?? 0) + 1
    }

    private var hasFeedback: Bool {
        !commentedBlocks.isEmpty || !general.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                header
                    .padding(.bottom, 24)
                content
                if case .question(let question) = item, question.origin != "plan", !question.options.isEmpty {
                    OptionGrid(options: question.options, picked: nil) { option in
                        choose(option, in: question)
                    }
                    .padding(.top, 28)
                }
                if loaded, loadError == nil {
                    commentsSummary
                        .padding(.top, 32)
                }
            }
            .padding(.horizontal, 20)
            .padding(.bottom, 24)
        }
        .scrollDismissesKeyboard(.interactively)
        .background(DawnBackground(glow: kind.tint))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(.hidden, for: .tabBar)
        .safeAreaBar(edge: .bottom) { actionBar }
        .toolbar {
            if case .report(let report) = item, let node = fleet.node(report.sessionId) {
                ToolbarItem(placement: .topBarTrailing) {
                    Menu {
                        Button(role: .destructive) {
                            closing = CloseRequest(session: node.sessionId, name: node.displayName, kind: .readAndClose)
                        } label: {
                            Label("Read and Close Node", systemImage: "xmark.bin")
                        }
                    } label: {
                        Label("More", systemImage: "ellipsis")
                    }
                    .disabled(sending)
                }
            }
        }
        .closeConfirmation($closing) { _ in
            guard case .report(let report) = item else { return }
            finish(HandoffLabel.readAndClosed) { try await $0.readAndClose(report) }
        }
        .sheet(item: $composing) { block in
            CommentSheet(block: block, initial: comments[block.draftKey] ?? "") { text in
                withAnimation(.spring(duration: 0.4, bounce: 0.25)) {
                    comments[block.draftKey] = text.isEmpty ? nil : text
                }
            }
        }
        .sensoryFeedback(.selection, trigger: composing?.id)
        .sensoryFeedback(.success, trigger: comments.count) { old, new in new > old }
        .task { await load() }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 12) {
            KindBadge(kind: kind)
            Text(item.title)
                .font(.display(.largeTitle, weight: .bold))
                .foregroundStyle(Palette.ink)
                .fixedSize(horizontal: false, vertical: true)
            HStack(spacing: 6) {
                if let name = item.nodeName {
                    Text(name).font(.mono(.footnote, weight: .medium))
                }
                Text(verbatim: "·")
                Text(relative(item.date))
            }
            .font(.footnote)
            .foregroundStyle(.secondary)
            if loaded, loadError == nil, !blocks.isEmpty {
                Label("Tap a block to comment on it", systemImage: "hand.tap")
                    .font(.footnote.weight(.medium))
                    .foregroundStyle(kind.tint)
                    .padding(.top, 4)
            }
        }
    }

    @ViewBuilder
    private var content: some View {
        if let loadError {
            ContentUnavailableView {
                Label("Couldn’t open it", systemImage: "doc.questionmark")
            } description: {
                Text(loadError)
            } actions: {
                Button("Try Again") { Task { await load() } }
                    .buttonStyle(.glass)
            }
            .frame(maxWidth: .infinity)
            .padding(.top, 40)
        } else if !loaded {
            VStack(alignment: .leading, spacing: 14) {
                ForEach(0..<6, id: \.self) { index in
                    RoundedRectangle(cornerRadius: 6)
                        .fill(Palette.hairline)
                        .frame(height: 14)
                        .frame(maxWidth: index.isMultiple(of: 3) ? 220 : .infinity, alignment: .leading)
                }
            }
            .phaseAnimator([0.45, 1]) { view, opacity in view.opacity(opacity) } animation: { _ in .easeInOut(duration: 0.8) }
        } else {
            LazyVStack(alignment: .leading, spacing: 6) {
                ForEach(MarkdownParser.segments(blocks)) { segment in
                    switch segment {
                    case .block(let block):
                        BlockRow(block: block, comment: comments[block.draftKey], tint: kind.tint) {
                            composing = block
                        }
                    case .table(let rows):
                        TableRows(rows: rows, comments: comments, tint: kind.tint) { composing = $0 }
                    }
                }
            }
        }
    }

    private var commentsSummary: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack {
                Text("Your comments")
                    .font(.display(.title3))
                Spacer()
                if !commentedBlocks.isEmpty {
                    Text(commentedBlocks.count, format: .number)
                        .font(.subheadline.monospacedDigit().weight(.bold))
                        .foregroundStyle(kind.tint)
                        .contentTransition(.numericText())
                }
            }
            if feedbackComments.isEmpty {
                Text("You haven’t commented on any block yet.")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
            }
            ForEach(commentedBlocks) { block in
                VStack(alignment: .leading, spacing: 6) {
                    Text("\(number(of: block)). “\(ReviewFeedback.quote(block.plainText))”")
                        .font(.footnote.italic())
                        .foregroundStyle(.secondary)
                        .lineLimit(3)
                    Text(comments[block.draftKey] ?? "")
                        .font(.subheadline)
                }
                .onTapGesture { composing = block }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(14)
                .background(Palette.card, in: .rect(cornerRadius: 14))
                .transition(.scale(scale: 0.95).combined(with: .opacity))
            }
            TextField("General comment (optional)", text: $general, axis: .vertical)
                .lineLimit(2...8)
                .focused($generalFocused)
                .padding(14)
                .background(Palette.card, in: .rect(cornerRadius: 14))
                .overlay(RoundedRectangle(cornerRadius: 14).strokeBorder(generalFocused ? kind.tint.opacity(0.6) : Palette.hairline))
        }
    }

    @ViewBuilder
    private var actionBar: some View {
        GlassEffectContainer(spacing: 12) {
            HStack(spacing: 12) {
                switch item {
                case .question(let question):
                    Button {
                        sendFeedback()
                    } label: {
                        Label(changeOption(question).map(AnswerOption.displayLabel) ?? String(localized: "Reply"), systemImage: "text.bubble")
                            .frame(maxWidth: .infinity)
                    }
                    .modifier(GlassEmphasis(prominent: hasFeedback))
                    .tint(Palette.dawn)
                    .disabled(!hasFeedback || sending)
                    if let approve = approveOption(question) {
                        Button {
                            finish(approve) { try await $0.answer(question.id, text: approve, option: approve) }
                        } label: {
                            Label(AnswerOption.displayLabel(approve), systemImage: "checkmark")
                                .frame(maxWidth: .infinity)
                        }
                        .buttonStyle(.glassProminent)
                        .tint(Palette.sageDeep)
                        .disabled(sending)
                    }
                case .pullRequest:
                    Button {
                        sendFeedback()
                    } label: {
                        Label("Request Changes", systemImage: "arrow.uturn.backward")
                            .frame(maxWidth: .infinity)
                    }
                    .modifier(GlassEmphasis(prominent: hasFeedback))
                    .tint(Palette.dawn)
                    .disabled(!hasFeedback || sending)
                case .report(let report):
                    Button {
                        sendFeedback()
                    } label: {
                        Label("Send", systemImage: "paperplane")
                            .frame(maxWidth: .infinity)
                    }
                    .modifier(GlassEmphasis(prominent: hasFeedback))
                    .tint(Palette.dawn)
                    .disabled(!hasFeedback || sending)
                    Button {
                        finish(HandoffLabel.read) { try await $0.markRead(report.id) }
                    } label: {
                        Label("Mark as Read", systemImage: "checkmark")
                            .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.glassProminent)
                    .tint(kind.tint == Palette.sky ? Palette.skyDeep : Palette.ember)
                    .disabled(sending)
                }
            }
            .controlSize(.large)
            .buttonBorderShape(.capsule)
            .font(.headline)
            .lineLimit(1)
            .minimumScaleFactor(0.75)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 8)
    }

    private func sendFeedback() {
        guard hasFeedback, !sending else { return }
        let text = feedbackText
        switch item {
        case .question(let question):
            let change = changeOption(question)
            finish(change ?? HandoffLabel.answered) { try await $0.answer(question.id, text: text, option: change) }
        case .report(let report):
            finish(HandoffLabel.sent) { client in
                try await client.send(to: report.sessionId, text: text)
                try await client.markRead(report.id)
            }
        case .pullRequest(let pr):
            finish(HandoffLabel.sent) { try await $0.requestChanges(onPullRequest: pr.id, text: text) }
        }
    }

    private var feedbackText: String {
        ReviewFeedback.format(feedbackComments, general: general, author: fleet.userName)
    }

    private func choose(_ option: QuestionOption, in question: Question) {
        if TriageCard.needsText(option), !hasFeedback {
            generalFocused = true
            return
        }
        let text = hasFeedback ? feedbackText : option.label
        finish(option.label) { try await $0.answer(question.id, text: text, option: option.label) }
    }

    private func approveOption(_ question: Question) -> String? {
        if question.origin == "plan" { return AnswerOption.approve }
        return question.options.map(\.label).first { $0.hasPrefix(AnswerOption.approve) || $0.hasPrefix("Aprobar") || $0 == AnswerOption.allow }
    }

    private func changeOption(_ question: Question) -> String? {
        if question.origin == "plan" { return AnswerOption.requestChanges }
        return question.options.map(\.label).first { $0.hasPrefix(AnswerOption.requestChanges) || $0.hasPrefix("Pedir cambios") || $0.hasPrefix("Cambiar") || $0 == AnswerOption.deny }
    }

    private func finish(_ label: String, _ action: @escaping @Sendable (AlondraClient) async throws -> Void) {
        sending = true
        let handoff = FleetStore.Handoff(itemID: item.id, label: label, action: action)
        let fleet = fleet
        dismiss()
        Task {
            try? await Task.sleep(for: .milliseconds(320))
            fleet.handoff = handoff
        }
    }

    private func load() async {
        loadError = nil
        do {
            let source = try await markdownSource()
            let parsed = MarkdownParser.blocks(from: source)
            withAnimation(.smooth(duration: 0.35)) {
                blocks = parsed
                loaded = true
            }
        } catch where error.isCancellation {
        } catch {
            withAnimation { loadError = error.friendlyDescription; loaded = true }
        }
    }

    private func markdownSource() async throws -> String {
        switch item {
        case .question(let question):
            if question.origin == "plan", let detail = question.detail { return detail }
            if let path = question.attachments.compactMap(\.storedPath).first {
                return try await fleet.client.fileText(path)
            }
            return question.readableDetail ?? ""
        case .report(let report):
            if let path = report.attachments.compactMap(\.storedPath).first {
                return try await fleet.client.fileText(path)
            }
            return report.body ?? ""
        case .pullRequest(let pr):
            return pr.body ?? ""
        }
    }
}

private struct BlockRow: View {
    let block: MarkdownBlock
    let comment: String?
    let tint: Color
    let onTap: () -> Void

    var body: some View {
        if block.isCommentable {
            Button(action: onTap) {
                VStack(alignment: .leading, spacing: 10) {
                    MarkdownBlockView(block: block)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    if let comment {
                        HStack(alignment: .top, spacing: 8) {
                            Image(systemName: "text.bubble.fill")
                                .foregroundStyle(tint)
                            Text(comment)
                                .font(.subheadline)
                                .foregroundStyle(Palette.ink)
                        }
                        .padding(12)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(tint.opacity(0.14), in: .rect(cornerRadius: 12))
                        .transition(.asymmetric(insertion: .scale(scale: 0.9, anchor: .top).combined(with: .opacity), removal: .opacity))
                    }
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 8)
                .background {
                    if comment != nil {
                        HStack(spacing: 0) {
                            Capsule().fill(tint).frame(width: 3)
                            Spacer()
                        }
                        .padding(.vertical, 4)
                        .transition(.opacity)
                    }
                }
                .contentShape(.rect(cornerRadius: 12))
            }
            .buttonStyle(BlockButtonStyle(tint: tint))
            .foregroundStyle(Palette.ink)
            .padding(.horizontal, -12)
            .accessibilityHint(comment == nil ? String(localized: "Add comment") : String(localized: "Edit comment"))
        } else {
            MarkdownBlockView(block: block)
        }
    }
}

private struct TableRows: View {
    let rows: [MarkdownBlock]
    let comments: [String: String]
    let tint: Color
    let onTap: (MarkdownBlock) -> Void

    var body: some View {
        TableBlock(rows: rows) { block, cell in
            let comment = comments[block.draftKey]
            Button { onTap(block) } label: {
                cell
                    .frame(maxHeight: .infinity, alignment: .top)
                    .background(comment == nil ? .clear : tint.opacity(0.14))
            }
            .buttonStyle(.plain)
            .accessibilityHint(comment == nil ? String(localized: "Add comment") : String(localized: "Edit comment"))
        } below: { block in
            if let comment = comments[block.draftKey] {
                bubble(comment)
                    .padding(.vertical, 4)
                    .gridCellUnsizedAxes(.horizontal)
                    .onTapGesture { onTap(block) }
            }
        } card: { block, card in
            let comment = comments[block.draftKey]
            Button { onTap(block) } label: {
                VStack(alignment: .leading, spacing: 6) {
                    card
                        .overlay {
                            if comment != nil {
                                RoundedRectangle(cornerRadius: 12).strokeBorder(tint, lineWidth: 1.5)
                            }
                        }
                    if let comment {
                        bubble(comment)
                    }
                }
                .contentShape(.rect(cornerRadius: 12))
            }
            .buttonStyle(BlockButtonStyle(tint: tint))
            .accessibilityHint(comment == nil ? String(localized: "Add comment") : String(localized: "Edit comment"))
        }
        .foregroundStyle(Palette.ink)
    }

    private func bubble(_ comment: String) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: "text.bubble.fill")
                .foregroundStyle(tint)
            Text(comment)
                .font(.subheadline)
                .foregroundStyle(Palette.ink)
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(tint.opacity(0.14), in: .rect(cornerRadius: 10))
    }
}

private struct BlockButtonStyle: ButtonStyle {
    let tint: Color

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .background(tint.opacity(configuration.isPressed ? 0.12 : 0), in: .rect(cornerRadius: 12))
            .scaleEffect(configuration.isPressed ? 0.985 : 1)
            .animation(.spring(duration: 0.25), value: configuration.isPressed)
    }
}

private struct CommentSheet: View {
    let block: MarkdownBlock
    let initial: String
    let onSave: (String) -> Void

    @Environment(\.dismiss) private var dismiss
    @State private var text = ""
    @FocusState private var focused: Bool

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    HStack(alignment: .top, spacing: 12) {
                        Capsule().fill(Palette.dawn).frame(width: 3)
                        Text(ReviewFeedback.quote(block.plainText))
                            .font(.callout.italic())
                            .foregroundStyle(.secondary)
                            .lineLimit(6)
                    }
                    .fixedSize(horizontal: false, vertical: true)
                    TextField("What would you change here?", text: $text, axis: .vertical)
                        .font(.body)
                        .lineLimit(4...12)
                        .focused($focused)
                }
                .padding(20)
            }
            .navigationTitle(initial.isEmpty ? String(localized: "Comment") : String(localized: "Edit Comment"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel", systemImage: "xmark") { dismiss() }
                }
                if !initial.isEmpty {
                    ToolbarItem(placement: .destructiveAction) {
                        Button("Delete", systemImage: "trash", role: .destructive) {
                            onSave("")
                            dismiss()
                        }
                    }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save", systemImage: "checkmark") {
                        onSave(text.trimmingCharacters(in: .whitespacesAndNewlines))
                        dismiss()
                    }
                    .disabled(text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
            }
        }
        .presentationDetents([.medium, .large])
        .presentationBackground(.regularMaterial)
        .onAppear {
            text = initial
            focused = true
        }
    }
}

private struct GlassEmphasis: ViewModifier {
    let prominent: Bool

    func body(content: Content) -> some View {
        if prominent {
            content.buttonStyle(.glassProminent)
        } else {
            content.buttonStyle(.glass)
        }
    }
}
