import AlondraShared
import SwiftUI
import ComposableArchitecture
import AlondraWorkspaceFeature
import AlondraQuestionsFeature
import AlondraAttachmentsFeature

struct ServiceTriageCard: View {
    @Bindable var store: StoreOf<TriageCardFeature>
    var item: InboxItem { store.item }
    let node: Node?
    let task: WorkTask?
    let isFront: Bool
    let zoom: Namespace.ID
    let onReview: () -> Void
    let onSkip: () -> Void
    let onHide: () -> Void
    let questions: [Question]
    let reports: [Report]

    @Environment(\.dynamicTypeSize) private var typeSize
    @State private var contentHeight: CGFloat?
    @FocusState private var replyFocused: Bool
    private var pickedOption: QuestionOption? { store.selectedOption }
    private var stamp: Stamp? {
        guard let resolution = store.stamp else { return nil }
        switch resolution {
        case .answer(let option):
            guard let option else { return Stamp(label: String(localized: "Answered"), symbol: "paperplane.fill", tint: Palette.sky) }
            switch ServiceOptionTone(option) {
            case .positive: return Stamp(label: AnswerOption.displayLabel(option), symbol: "checkmark", tint: Palette.sage)
            case .negative: return Stamp(label: AnswerOption.displayLabel(option), symbol: "xmark", tint: Palette.rose)
            case .neutral: return Stamp(label: AnswerOption.displayLabel(option), symbol: "paperplane.fill", tint: kind.tint)
            }
        case .sent: return Stamp(label: String(localized: "Sent"), symbol: "paperplane.fill", tint: Palette.sky)
        case .contextRequested: return Stamp(label: String(localized: "Context requested"), symbol: "questionmark", tint: Palette.slate)
        case .commentsSent: return Stamp(label: String(localized: "Comments sent"), symbol: "text.bubble.fill", tint: Palette.sky)
        case .approved: return Stamp(label: String(localized: "Approved"), symbol: "checkmark.seal.fill", tint: actionTint)
        case .changesRequested: return Stamp(label: String(localized: "Changes requested"), symbol: "arrow.uturn.backward", tint: Palette.dawn)
        case .merged: return Stamp(label: String(localized: "Merged"), symbol: "arrow.triangle.merge", tint: Palette.sage)
        case .conflictsSent: return Stamp(label: String(localized: "Sent to its node"), symbol: "arrow.triangle.branch", tint: Palette.dawn)
        case .setAside: return Stamp(label: String(localized: "Set aside"), symbol: "tray.and.arrow.down", tint: Palette.slate)
        }
    }
    private var showNodeContext: Bool { store.showNodeContext }

    private var kind: Kind { Kind(item) }
    private var actionTint: Color { kind.tint == Palette.slate ? Palette.dawn : kind.tint }

    private struct Stamp: Equatable {
        let label: String
        let symbol: String
        let tint: Color
    }

    var body: some View {
        layout
            .blur(radius: stamp == nil ? 0 : 10)
            .opacity(stamp == nil ? 1 : 0.4)
            .frame(maxWidth: .infinity)
            .background(cardBackground)
            .clipShape(.rect(cornerRadius: 34, style: .continuous))
            .overlay {
                RoundedRectangle(cornerRadius: 34, style: .continuous)
                    .strokeBorder(Palette.hairline)
            }
            .overlay { stampOverlay }
            .shadow(color: .black.opacity(isFront ? 0.22 : 0.08), radius: isFront ? 24 : 8, y: isFront ? 14 : 4)
            .scaleEffect(stamp == nil ? 1 : 1.02)
            .alert("What do you need to decide?", isPresented: Binding(get: { store.askingContext }, set: { _ = store.send(.contextPresented($0)) })) {
                TextField("Optional", text: Binding(get: { store.contextNote }, set: { _ = store.send(.contextChanged($0)) }))
                Button("Cancel", role: .cancel) {}
                Button("Ask for Context") { _ = store.send(.requestContext) }
            } message: {
                Text("The node will explain and ask again.")
            }
            .onChange(of: store.question?.replyFocusRequest) { _, _ in replyFocused = true }
            .accessibilityAction(named: "Leave for Later", onSkip)
            .task { await store.send(.appeared).finish() }

    }

    @ViewBuilder
    private var layout: some View {
        if typeSize.isAccessibilitySize {
            ScrollView {
                VStack(spacing: 0) {
                    content
                    actions.disabled(store.busy)
                        .padding(.horizontal, 16)
                        .padding(.bottom, 16)
                }
            }
            .scrollBounceBehavior(.basedOnSize)
            .scrollDismissesKeyboard(.interactively)
        } else {
            VStack(spacing: 0) {
                ScrollView { content }
                    .scrollBounceBehavior(.basedOnSize)
                    .scrollDismissesKeyboard(.interactively)
                    .onScrollGeometryChange(for: CGFloat.self) { $0.contentSize.height } action: { _, height in
                        contentHeight = height
                    }
                    .frame(maxHeight: contentHeight ?? .infinity)
                    .mask(fadeMask)
                actions.disabled(store.busy)
                    .padding(.horizontal, 16)
                    .padding(.bottom, 16)
                    .padding(.top, 4)
            }
        }
    }

    private var content: some View {
        VStack(alignment: .leading, spacing: 16) {
            topLine
            if let notice = store.notice { Text(notice).font(.caption).foregroundStyle(Palette.rose) }
            Text(item.title)
                .font(.display(.title2, weight: .bold))
                .foregroundStyle(Palette.ink)
                .fixedSize(horizontal: false, vertical: true)
            nodeLine
            if let task = task {
                Label {
                    Text(task.title)
                } icon: {
                    Image(systemName: "checklist")
                        .font(.subheadline)
                }
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .accessibilityLabel(Text("Task: \(task.title)"))
            }
            bodyContent
        }
        .padding(.horizontal, 22)
        .padding(.top, 22)
        .padding(.bottom, 16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(.rect)
        .onTapGesture(perform: onReview)
        .accessibilityAction(named: "Open", onReview)
    }

    private var cardBackground: some View {
        ZStack(alignment: .top) {
            Palette.card
            LinearGradient(colors: [kind.tint.opacity(0.16), kind.tint.opacity(0)], startPoint: .top, endPoint: .bottom)
                .frame(height: 180)
        }
    }

    private var fadeMask: some View {
        VStack(spacing: 0) {
            Color.black
            LinearGradient(colors: [.black, .black.opacity(0)], startPoint: .top, endPoint: .bottom)
                .frame(height: 22)
        }
    }

    private var topLine: some View {
        HStack(alignment: .center, spacing: 8) {
            KindBadge(kind: kind)
            Spacer()
            Text(relative(item.date))
                .font(.footnote)
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .fixedSize()
            Button(action: onSkip) {
                Image(systemName: "arrow.uturn.forward")
                    .font(.footnote.weight(.semibold))
                    .frame(width: 30, height: 30)
                    .background(Palette.hairline, in: .circle)
            }
            .buttonStyle(.plain)
            .foregroundStyle(.secondary)
            .accessibilityLabel("Leave for Later")
        }
    }

    private var nodeLine: some View {
        return HStack(spacing: 8) {
            if let task = task {
                Glyph(task: task, size: .small)
            } else if let node {
                Image(systemName: node.state.symbol)
                    .font(.caption2.weight(.bold))
                    .foregroundStyle(node.state.tint)
                    .frame(width: 22, height: 22)
                    .background(node.state.tint.opacity(0.15), in: .circle)
            }
            Text(item.nodeName ?? node?.displayName ?? String(localized: "node"))
                .font(.mono(.subheadline, weight: .medium))
                .foregroundStyle(Palette.ink.opacity(0.85))
            if let project = node?.projectName {
                Text(project)
                    .font(.caption.weight(.medium))
                    .foregroundStyle(.secondary)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 3)
                    .background(Palette.hairline, in: .capsule)
            }
        }
        .lineLimit(1)
    }

    @ViewBuilder
    private var bodyContent: some View {
        if let mission = item.missionExcerpt {
            (Text("Mission").fontWeight(.semibold) + Text(verbatim: " · \(mission)"))
                .font(.footnote)
                .foregroundStyle(.secondary)
                .lineLimit(3)
        }
        switch item {
        case .question(let question):
            if question.origin == "plan", let plan = question.detail {
                excerpt(plan, cta: String(localized: "Review the plan"))
            } else {
                if let detail = question.readableDetail, !detail.isEmpty {
                    if question.origin == "permission" {
                        Text(detail)
                            .font(.mono(.callout))
                            .foregroundStyle(Palette.ink)
                            .padding(14)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .background(Palette.night.opacity(0.7), in: .rect(cornerRadius: 14, style: .continuous))
                            .textSelection(.enabled)
                    } else if detail.count > 600 {
                        excerpt(detail, cta: String(localized: "Read it all"))
                    } else {
                        blocks(detail)
                    }
                }
                ForEach(question.attachments.filter { $0.storedPath != nil }, id: \.sourcePath) { attachment in
                    reviewButton(String(localized: "Review the attachment"), detail: attachment.name, symbol: "doc.text.magnifyingglass")
                }
            }
            links(question.links)
            ForEach(question.aboutReports) { report in
                aboutReport(report)
            }
            if let context = question.nodeContext?.trimmingCharacters(in: .whitespacesAndNewlines), !context.isEmpty, !context.hasPrefix("mcp__") {
                nodeContext(context)
            }
        case .report(let report):
            if report.seemsToAsk {
                Label(Report.seemsToAskLabel, systemImage: "questionmark.bubble")
                    .font(.footnote.weight(.semibold))
                    .foregroundStyle(Palette.dawn)
            }
            ForEach(report.artifacts, id: \.self) { url in
                artifactButton(url)
            }
            if let body = report.body, !body.isEmpty {
                if report.attachments.isEmpty && body.count > 600 {
                    excerpt(body, cta: String(localized: "Read it all"))
                } else {
                    blocks(body)
                }
            }
            ForEach(report.attachments.filter { $0.storedPath != nil }, id: \.sourcePath) { attachment in
                reviewButton(String(localized: "Read the document"), detail: attachment.name, symbol: "doc.richtext")
            }
            links(report.links)
            if let earlier = report.earlier, !earlier.isEmpty {
                earlierReports(earlier)
            }
        case .pullRequest(let pr):
            ForEach(reports) { report in
                AttachedReport(report: report)
            }
            ForEach(questions) { question in
                AttachedQuestion(question: question)
            }
            Text(pr.summary)
                .font(.footnote.monospaced())
                .foregroundStyle(.secondary)
            PRStatus(pr: pr)
            if let body = pr.body, !body.isEmpty {
                if body.count > 600 {
                    excerpt(body, cta: String(localized: "Read it all"))
                } else {
                    blocks(body)
                }
            }
        }
    }

    private func blocks(_ markdown: String) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            ForEach(MarkdownParser.segments(from: markdown)) { segment in
                MarkdownSegmentView(segment: segment)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .foregroundStyle(Palette.ink.opacity(0.9))
    }

    private func excerpt(_ markdown: String, cta: String) -> some View {
        let blocks = MarkdownParser.blocks(from: markdown)
        let segments = MarkdownParser.segments(blocks.prefix(5))
        return Button(action: onReview) {
            VStack(alignment: .leading, spacing: 0) {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(segments) { segment in
                        MarkdownSegmentView(segment: segment)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
                .fixedSize(horizontal: false, vertical: true)
                .font(.callout)
                .foregroundStyle(Palette.ink.opacity(0.9))
                .padding(16)
                .frame(maxWidth: .infinity, maxHeight: 132, alignment: .topLeading)
                .clipped()
                .mask(LinearGradient(stops: [.init(color: .black, location: 0.45), .init(color: .clear, location: 0.9)], startPoint: .top, endPoint: .bottom))
                HStack(spacing: 10) {
                    Image(systemName: "text.viewfinder")
                        .font(.title3)
                    VStack(alignment: .leading, spacing: 1) {
                        Text(cta)
                            .font(.headline)
                            .foregroundStyle(Palette.ink)
                        Text("\(blocks.count) blocks · tap one to comment on it")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    Spacer()
                    Image(systemName: "chevron.right")
                        .font(.footnote.weight(.bold))
                        .foregroundStyle(.tertiary)
                }
                .foregroundStyle(kind.tint)
                .padding(.horizontal, 16)
                .padding(.vertical, 12)
                .background(kind.tint.opacity(0.10))
                .overlay(alignment: .top) {
                    Rectangle()
                        .fill(kind.tint.opacity(0.25))
                        .frame(height: 1)
                }
            }
            .background(Palette.cardRaised.opacity(0.6))
            .clipShape(.rect(cornerRadius: 20, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 20, style: .continuous).strokeBorder(kind.tint.opacity(0.25)))
            .contentShape(.rect(cornerRadius: 20))
        }
        .buttonStyle(PressStyle())
        .matchedTransitionSource(id: item.id, in: zoom)
    }

    private func artifactButton(_ url: URL) -> some View {
        Link(destination: url) {
            HStack(spacing: 12) {
                Image(systemName: "safari")
                    .font(.title3)
                    .foregroundStyle(kind.tint)
                VStack(alignment: .leading, spacing: 2) {
                    Text(Report.openInBrowserLabel)
                        .font(.headline)
                        .foregroundStyle(Palette.ink)
                    Text(url.absoluteString)
                        .font(.caption)
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Image(systemName: "arrow.up.right")
                    .font(.footnote.weight(.bold))
                    .foregroundStyle(.tertiary)
            }
            .padding(14)
            .background(kind.tint.opacity(0.10), in: .rect(cornerRadius: 18, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 18, style: .continuous).strokeBorder(kind.tint.opacity(0.25)))
            .contentShape(.rect(cornerRadius: 18))
        }
        .buttonStyle(.plain)
    }

    private func reviewButton(_ title: String, detail: String, symbol: String) -> some View {
        Button(action: onReview) {
            HStack(spacing: 12) {
                Image(systemName: symbol)
                    .font(.title3)
                    .foregroundStyle(kind.tint)
                VStack(alignment: .leading, spacing: 2) {
                    Text(title)
                        .font(.headline)
                        .foregroundStyle(Palette.ink)
                        .lineLimit(2)
                    Text(detail)
                        .font(.caption)
                        .lineLimit(1)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Image(systemName: "chevron.right")
                    .font(.footnote.weight(.bold))
                    .foregroundStyle(.tertiary)
            }
            .padding(14)
            .background(kind.tint.opacity(0.10), in: .rect(cornerRadius: 18, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 18, style: .continuous).strokeBorder(kind.tint.opacity(0.25)))
            .contentShape(.rect(cornerRadius: 18))
        }
        .buttonStyle(PressStyle())
        .matchedTransitionSource(id: item.id, in: zoom)
    }

    @ViewBuilder
    private func links(_ links: [LinkRef]) -> some View {
        ForEach(links, id: \.url) { link in
            if let url = URL(string: link.url) {
                Link(destination: url) {
                    Label(link.title ?? link.url, systemImage: "link")
                        .font(.subheadline.weight(.medium))
                        .lineLimit(1)
                }
                .tint(Palette.sky)
            }
        }
    }

    private func earlierReports(_ earlier: [Report]) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Label(String(localized: "Earlier in this delivery (\(earlier.count))"), systemImage: "tray.full")
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(.secondary)
            ForEach(earlier) { e in
                VStack(alignment: .leading, spacing: 4) {
                    Text(e.title)
                        .font(.headline)
                        .foregroundStyle(Palette.ink)
                    if let body = e.body, !body.isEmpty {
                        blocks(body)
                            .font(.callout)
                            .lineLimit(8)
                    }
                }
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Palette.hairline.opacity(0.5), in: .rect(cornerRadius: 14))
    }

    private func aboutReport(_ report: Report) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Label(String(localized: "About report #\(report.id)"), systemImage: "doc.text")
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(.secondary)
            Text(report.title)
                .font(.headline)
                .foregroundStyle(Palette.ink)
            if let body = report.body, !body.isEmpty {
                blocks(body)
                    .font(.callout)
                    .lineLimit(12)
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Palette.hairline.opacity(0.5), in: .rect(cornerRadius: 14))
    }

    private func nodeContext(_ context: String) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Button {
                withAnimation(.spring(duration: 0.35)) { _ = store.send(.toggleNodeContext) }
            } label: {
                HStack {
                    Label("What the node said last", systemImage: "text.quote")
                    Spacer()
                    Image(systemName: "chevron.down")
                        .rotationEffect(.degrees(showNodeContext ? 180 : 0))
                }
                .font(.subheadline.weight(.medium))
                .foregroundStyle(.secondary)
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
            if showNodeContext {
                blocks(context)
                    .font(.callout)
                    .padding(14)
                    .background(Palette.hairline.opacity(0.5), in: .rect(cornerRadius: 14))
                    .transition(.opacity.combined(with: .move(edge: .top)))
            }
        }
        .padding(.top, 4)
    }

    @ViewBuilder
    private var actions: some View {
        switch item {
        case .question(let question):
            VStack(spacing: 8) {
                ServiceOptionGrid(options: question.options, picked: pickedOption) { option in
                    store.send(.choose(option))
                }
                replyField(placeholder: pickedOption?.displayDescription ?? String(localized: "Answer in your own words…")) {
                    store.send(.send)
                }
                Button {
                    store.send(.contextChanged(""))
                    store.send(.contextPresented(true))
                } label: {
                    Label("I need more context", systemImage: "questionmark.bubble")
                        .font(.subheadline.weight(.medium))
                        .foregroundStyle(.secondary)
                        .frame(minHeight: 30)
                }
                .buttonStyle(.plain)
            }
        case .report(let report):
            VStack(spacing: 10) {
                replyField(placeholder: String(localized: "Reply to \(item.nodeName ?? String(localized: "this node"))…")) {
                    store.send(.send)
                }
                if report.primaryAction() == .review {
                    Button {
                        store.send(.document(.reviewTapped(.comments)))
                    } label: {
                        Label("Rescue Comments", systemImage: "text.bubble")
                            .font(.headline)
                            .frame(maxWidth: .infinity, minHeight: 50)
                    }
                    .buttonStyle(ServiceFilledStyle(tint: actionTint))
                    HStack(spacing: 24) {
                        Button {
                            store.send(.document(.reviewTapped(.approved)))
                        } label: {
                            Label("Approve", systemImage: "checkmark.seal")
                                .font(.subheadline.weight(.medium))
                                .frame(minHeight: 30)
                        }
                        Button {
                            store.send(.markRead)
                        } label: {
                            Label("Mark as Read", systemImage: "checkmark")
                                .font(.subheadline.weight(.medium))
                                .frame(minHeight: 30)
                        }
                    }
                    .foregroundStyle(.secondary)
                    .buttonStyle(.plain)
                } else {
                    Button {
                        store.send(.markRead)
                    } label: {
                        Label("Mark as Read", systemImage: "checkmark")
                            .font(.headline)
                            .frame(maxWidth: .infinity, minHeight: 50)
                    }
                    .buttonStyle(ServiceFilledStyle(tint: actionTint))
                }
            }
        case .pullRequest(let pr):
            VStack(spacing: 10) {
                replyField(placeholder: String(localized: "What should change?")) {
                    store.send(.send)
                }
                Button {
                    store.send(.ask)
                } label: {
                    Label("Ask", systemImage: "questionmark.bubble")
                        .font(.subheadline.weight(.medium))
                        .foregroundStyle(.secondary)
                        .frame(minHeight: 30)
                }
                .buttonStyle(.plain)
                .disabled(!store.canSend || store.busy)
                if let child = store.scope(state: \.pullRequest, action: \.pullRequest) {
                    ServicePRMainAction(store: child.scope(state: \.pr, action: \.pr), placement: .card)
                }
                PRFooter(pr: pr) { _ = store.send(.pullRequest(.pr(.setAsideTapped))) }
            }
        }
    }

    private func replyField(placeholder: String, onSend: @escaping () -> Void) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            if let pickedOption {
                HStack(spacing: 6) {
                    Text(pickedOption.displayLabel)
                        .font(.caption.weight(.bold))
                    Button {
                        withAnimation(.spring(duration: 0.3)) { _ = store.send(.clearOption) }
                    } label: {
                        Image(systemName: "xmark.circle.fill")
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Remove Option")
                }
                .foregroundStyle(kind.tint)
                .padding(.horizontal, 10)
                .padding(.vertical, 5)
                .background(kind.tint.opacity(0.14), in: .capsule)
                .transition(.scale(scale: 0.8, anchor: .leading).combined(with: .opacity))
            }
            if let attachmentStore { ServiceAttachmentStrip(store: attachmentStore) }
            HStack(alignment: .bottom, spacing: 8) {
                if let attachmentStore {
                    ServiceAttachButton(store: attachmentStore).padding(.leading, 6).padding(.bottom, 4)
                }
                TextField(placeholder, text: Binding(get: { store.reply }, set: { _ = store.send(.replyChanged($0)) }), axis: .vertical)
                    .lineLimit(1...5)
                    .focused($replyFocused)
                    .padding(.trailing, 16)
                    .padding(.vertical, 12)
                let empty = !store.canSend
                Button(action: onSend) {
                    Image(systemName: "arrow.up")
                        .font(.body.weight(.bold))
                        .foregroundStyle(Palette.onAccent)
                        .frame(width: 36, height: 36)
                        .background(empty ? Palette.slate.opacity(0.4) : kind.tint, in: .circle)
                }
                .buttonStyle(ServicePressStyle())
                .disabled(empty)
                .padding(4)
                .accessibilityLabel("Send")
                .animation(.spring(duration: 0.3), value: empty)
            }
            .background(Palette.cardRaised, in: .rect(cornerRadius: 22, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 22, style: .continuous).strokeBorder(replyFocused ? kind.tint.opacity(0.5) : Palette.hairline))
        }
    }

    private var attachmentStore: StoreOf<AttachmentFeature>? {
        if let child = store.scope(state: \.question, action: \.question), !child.question.isTerminalMenu { return child.scope(state: \.attachmentPicker, action: \.attachmentPicker) }
        if let child = store.scope(state: \.document, action: \.document) { return child.scope(state: \.attachmentPicker, action: \.attachmentPicker) }
        if let child = store.scope(state: \.pullRequest, action: \.pullRequest) { return child.scope(state: \.diff.attachmentPicker, action: \.diff.attachmentPicker) }
        return nil
    }

    @ViewBuilder
    private var stampOverlay: some View {
        if let stamp {
            ZStack {
                RadialGradient(colors: [stamp.tint.opacity(0.35), stamp.tint.opacity(0.05)], center: .center, startRadius: 10, endRadius: 260)
                    .clipShape(.rect(cornerRadius: 34, style: .continuous))
                VStack(spacing: 10) {
                    Image(systemName: stamp.symbol)
                        .font(.system(size: 44, weight: .bold))
                        .foregroundStyle(Palette.onAccent)
                        .frame(width: 92, height: 92)
                        .background(stamp.tint, in: .circle)
                        .shadow(color: stamp.tint.opacity(0.5), radius: 20)
                        .symbolEffect(.bounce, value: stamp)
                    Text(stamp.label)
                        .font(.display(.title2, weight: .bold))
                        .foregroundStyle(Palette.ink)
                }
                .transition(.scale(scale: 0.4).combined(with: .opacity))
            }
            .transition(.opacity)
        }
    }

}
enum ServiceOptionTone {
    case positive, negative, neutral

    init(_ label: String) {
        let lower = label.lowercased()
        if label == AnswerOption.done {
            self = .positive
        } else if label == AnswerOption.cannot {
            self = .negative
        } else if ["aprobar", "permitir", "sí", "si", "vale", "adelante", "ok", "approve", "allow", "yes"].contains(where: { lower.hasPrefix($0) }) {
            self = .positive
        } else if ["denegar", "no", "cancelar", "rechazar", "deny", "reject", "cancel"].contains(where: { lower == $0 || lower.hasPrefix("\($0) ") }) {
            self = .negative
        } else {
            self = .neutral
        }
    }
}

struct ServiceOptionGrid: View {
    let options: [QuestionOption]
    let picked: QuestionOption?
    let onPick: (QuestionOption) -> Void

    @Environment(\.dynamicTypeSize) private var typeSize

    private var compact: Bool {
        options.count == 2 && options.allSatisfy { $0.displayLabel.count <= 16 && (needsText($0) || $0.description == nil) } && !typeSize.isAccessibilitySize
    }

    var body: some View {
        let layout = compact ? AnyLayout(HStackLayout(spacing: 10)) : AnyLayout(VStackLayout(spacing: 8))
        layout {
            ForEach(options, id: \.label) { option in
                Button { onPick(option) } label: {
                    VStack(alignment: compact ? .center : .leading, spacing: 2) {
                        Text(option.displayLabel)
                            .font(.headline)
                        if let description = option.displayDescription, !compact, !needsText(option) {
                            Text(description)
                                .font(.caption)
                                .opacity(0.8)
                                .multilineTextAlignment(.leading)
                        }
                    }
                    .frame(maxWidth: .infinity, minHeight: 46, alignment: compact ? .center : .leading)
                    .padding(.horizontal, compact ? 0 : 6)
                }
                .buttonStyle(ServiceOptionStyle(tone: ServiceOptionTone(option.label), picked: picked == option))
            }
        }
    }
}

private struct ServiceOptionStyle: ButtonStyle {
    let tone: ServiceOptionTone
    let picked: Bool

    func makeBody(configuration: Configuration) -> some View {
        let tint: Color = switch tone {
        case .positive: Palette.sage
        case .negative: Palette.rose
        case .neutral: Palette.dawn
        }
        let filled = tone == .positive || picked
        configuration.label
            .foregroundStyle(filled ? Palette.onAccent : tint)
            .padding(.horizontal, 14)
            .padding(.vertical, 4)
            .background(filled ? tint : tint.opacity(0.12), in: .rect(cornerRadius: 18, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 18, style: .continuous).strokeBorder(tint.opacity(filled ? 0 : 0.35)))
            .scaleEffect(configuration.isPressed ? 0.96 : 1)
            .animation(.spring(duration: 0.25, bounce: 0.4), value: configuration.isPressed)
            .animation(.spring(duration: 0.3), value: picked)
    }
}

struct ServiceFilledStyle: ButtonStyle {
    let tint: Color

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .foregroundStyle(Palette.onAccent)
            .background(tint, in: .rect(cornerRadius: 18, style: .continuous))
            .scaleEffect(configuration.isPressed ? 0.97 : 1)
            .animation(.spring(duration: 0.25, bounce: 0.4), value: configuration.isPressed)
    }
}

struct ServicePressStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .scaleEffect(configuration.isPressed ? 0.96 : 1)
            .opacity(configuration.isPressed ? 0.85 : 1)
            .animation(.spring(duration: 0.25, bounce: 0.4), value: configuration.isPressed)
    }
}
