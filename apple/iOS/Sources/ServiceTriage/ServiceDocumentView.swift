import AlondraShared
import AlondraQuestionsFeature
import ComposableArchitecture
import SwiftUI

struct ServiceDocumentView: View {
    let store: StoreOf<DocumentReviewFeature>
    var markRead: (() -> Void)?
    @Environment(\.dismiss) private var dismiss
    @FocusState private var generalFocused: Bool
    private var item: InboxItem {
        switch store.subject { case .question(let q): .question(q); case .report(let r): .report(r) }
    }
    private var kind: Kind { Kind(item) }
    private var blocks: [MarkdownBlock] { MarkdownParser.blocks(from: store.sourceText ?? "") }
    private var loaded: Bool { store.sourceText != nil || store.loadError != nil }
    private var loadError: String? { store.loadError }
    private var comments: [String: String] { Dictionary(uniqueKeysWithValues: store.draft.comments.map { ($0.anchor, $0.text) }) }
    private var commentedBlocks: [MarkdownBlock] { blocks.filter { comments[$0.draftKey] != nil } }
    private var composing: MarkdownBlock? { blocks.first { $0.draftKey == store.composingAnchor } }
    private var hasFeedback: Bool { store.hasFeedback }
    private var sending: Bool { store.isSubmitting || store.needsReconciliation || store.completed || store.pendingSubmission != nil || !store.attachmentPicker.canPrepareSubmission }
    private func number(of block: MarkdownBlock) -> Int {
        (commentedBlocks.firstIndex(of: block) ?? 0) + 1
    }

    var body: some View {
        documentContent
        .scrollDismissesKeyboard(.interactively)
        .background(DawnBackground(glow: kind.tint))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(.hidden, for: .tabBar)
        .safeAreaBar(edge: .bottom) { actionBar }
        .sheet(item: Binding(get: { composing }, set: { _ = store.send(.composeComment($0?.draftKey)) })) { block in
            ServiceCommentSheet(block: block, initial: comments[block.draftKey] ?? "", text: Binding(get: { store.composingText }, set: { _ = store.send(.composingTextChanged($0)) })) { text in
                withAnimation(.spring(duration: 0.4, bounce: 0.25)) {
                    _ = store.send(.commentChanged(anchor: block.draftKey, quote: block.plainText, text: text))
                }
            }
        }
        .alert("Check Alondra", isPresented: Binding(get: { !store.noticeIsSubmissionFailure && (store.notice != nil || store.conflict != nil) }, set: { if !$0 { _ = store.send(.dismissNotice) } })) {
            if store.feedbackReadRetryAvailable { Button("Retry marking report read") { _ = store.send(.retryFeedbackRead) } }
            Button("OK") { _ = store.send(.dismissNotice) }
        } message: { Text(store.conflict ?? store.notice ?? "") }
        .sensoryFeedback(.selection, trigger: composing?.id)
        .sensoryFeedback(.success, trigger: comments.count) { old, new in new > old }
        .task { await store.send(.phoneAppeared).finish() }
        .onDisappear { _ = store.send(.disappeared) }
        .onChange(of: store.generalFocusRequest) { _, _ in generalFocused = true }
        .onChange(of: store.completed) { _, completed in if completed { dismiss() } }
    }

    private var documentContent: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                header
                    .padding(.bottom, 24)
                content
                if case .question(let question) = item, question.origin != "plan", !question.options.isEmpty {
                    ServiceOptionGrid(options: question.options, picked: nil) { option in
                        store.send(.chooseReviewOption(option, author: store.feedbackAuthor))
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
                Button("Try Again") { _ = store.send(.phoneAppeared) }
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
                        ServiceBlockRow(block: block, comment: comments[block.draftKey], tint: kind.tint) {
                            store.send(.composeComment(block.draftKey))
                        }
                    case .table(let rows):
                        ServiceTableRows(rows: rows, comments: comments, tint: kind.tint) { _ = store.send(.composeComment($0.draftKey)) }
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
            if store.draft.comments.isEmpty {
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
                .onTapGesture { _ = store.send(.composeComment(block.draftKey)) }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(14)
                .background(Palette.card, in: .rect(cornerRadius: 14))
                .transition(.scale(scale: 0.95).combined(with: .opacity))
            }
            TextField("General comment (optional)", text: Binding(get: { store.draft.general }, set: { _ = store.send(.generalChanged($0)) }), axis: .vertical)
                .lineLimit(2...8)
                .focused($generalFocused)
                .padding(14)
                .background(Palette.card, in: .rect(cornerRadius: 14))
                .overlay(RoundedRectangle(cornerRadius: 14).strokeBorder(generalFocused ? kind.tint.opacity(0.6) : Palette.hairline))
            HStack(alignment: .center, spacing: 8) {
                ServiceAttachButton(store: store.scope(state: \.attachmentPicker, action: \.attachmentPicker))
                ServiceAttachmentStrip(store: store.scope(state: \.attachmentPicker, action: \.attachmentPicker))
            }
        }
    }

    private var reportTint: Color {
        kind.tint == Palette.sky ? Palette.skyDeep : Palette.ember
    }

    @ViewBuilder
    private var actionBar: some View {
        GlassEffectContainer(spacing: 12) {
            VStack(spacing: 10) {
                HStack(spacing: 12) {
                    switch item {
                    case .question(let question):
                        Button {
                            store.send(.submitFeedback(author: store.feedbackAuthor))
                        } label: {
                            Label(store.changeOption.map(AnswerOption.displayLabel) ?? String(localized: "Reply"), systemImage: "text.bubble")
                                .frame(maxWidth: .infinity)
                        }
                        .modifier(ServiceGlassEmphasis(prominent: hasFeedback))
                        .tint(Palette.dawn)
                        .disabled(!hasFeedback || sending)
                        if let approve = store.approveOption {
                            Button {
                                store.send(.approveQuestion)
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
                            store.send(.submitFeedback(author: store.feedbackAuthor))
                        } label: {
                            Label("Request Changes", systemImage: "arrow.uturn.backward")
                                .frame(maxWidth: .infinity)
                        }
                        .modifier(ServiceGlassEmphasis(prominent: hasFeedback))
                        .tint(Palette.dawn)
                        .disabled(!hasFeedback || sending)
                    case .report:
                        Button {
                            store.send(.submitFeedback(author: store.feedbackAuthor))
                        } label: {
                            Label("Send", systemImage: "paperplane")
                                .frame(maxWidth: .infinity)
                        }
                        .modifier(ServiceGlassEmphasis(prominent: hasFeedback))
                        .tint(Palette.dawn)
                        .disabled(!hasFeedback || sending)
                        Button {
                            markRead?()
                        } label: {
                            Label("Mark as Read", systemImage: "checkmark")
                                .frame(maxWidth: .infinity)
                        }
                        .modifier(ServiceGlassEmphasis(prominent: true))
                        .tint(reportTint)
                        .disabled(sending)
                    }
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


}

private struct ServiceBlockRow: View {
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

private struct ServiceTableRows: View {
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

private struct ServiceCommentSheet: View {
    let block: MarkdownBlock
    let initial: String
    let onSave: (String) -> Void

    @Environment(\.dismiss) private var dismiss
    @Binding var text: String
    @FocusState private var focused: Bool

    init(block: MarkdownBlock, initial: String, text: Binding<String>, onSave: @escaping (String) -> Void) {
        self.block = block
        self.initial = initial
        self._text = text
        self.onSave = onSave
    }

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
            focused = true
        }
    }
}

private struct ServiceGlassEmphasis: ViewModifier {
    let prominent: Bool

    func body(content: Content) -> some View {
        if prominent {
            content.buttonStyle(.glassProminent)
        } else {
            content.buttonStyle(.glass)
        }
    }
}
