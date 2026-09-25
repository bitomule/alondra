import AlondraShared
import SwiftUI

struct TriageCard: View {
    let item: InboxItem
    let isFront: Bool
    let zoom: Namespace.ID
    let onReview: () -> Void
    let onSkip: () -> Void

    @Environment(FleetStore.self) private var fleet
    @Environment(\.dynamicTypeSize) private var typeSize
    @State private var reply = ""
    @State private var attachments = Attachments()
    @State private var pickedOption: QuestionOption?
    @State private var stamp: Stamp?
    @State private var askingContext = false
    @State private var contextNote = ""
    @State private var showNodeContext = false
    @State private var contentHeight: CGFloat?
    @FocusState private var replyFocused: Bool
    @State private var asking = false

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
            .alert("What do you need to decide?", isPresented: $askingContext) {
                TextField("Optional", text: $contextNote)
                Button("Cancel", role: .cancel) {}
                Button("Ask for Context") { requestContext() }
            } message: {
                Text("The node will explain and ask again.")
            }
            .accessibilityAction(named: "Leave for Later", onSkip)
            .onChange(of: fleet.handoff?.id, initial: true) {
                guard let handoff = fleet.handoff, handoff.itemID == item.id else { return }
                fleet.handoff = nil
                resolve(stamp(for: handoff.label), kind: handoff.kind, handoff.action)
            }
    }

    @ViewBuilder
    private var layout: some View {
        if typeSize.isAccessibilitySize {
            ScrollView {
                VStack(spacing: 0) {
                    content
                    actions
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
                actions
                    .padding(.horizontal, 16)
                    .padding(.bottom, 16)
                    .padding(.top, 4)
            }
        }
    }

    private var content: some View {
        VStack(alignment: .leading, spacing: 16) {
            topLine
            Text(item.title)
                .font(.display(.title2, weight: .bold))
                .foregroundStyle(Palette.ink)
                .fixedSize(horizontal: false, vertical: true)
            nodeLine
            if let task = fleet.task(for: item) {
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
        let node = fleet.node(item.sessionId)
        return HStack(spacing: 8) {
            if let task = fleet.task(for: item) {
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
            ForEach(fleet.reports(forPullRequest: pr.id)) { report in
                AttachedReport(report: report)
            }
            ForEach(fleet.questions(forPullRequest: pr.id)) { question in
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
                withAnimation(.spring(duration: 0.35)) { showNodeContext.toggle() }
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
                OptionGrid(options: question.options, picked: pickedOption) { option in
                    choose(option, in: question)
                }
                replyField(placeholder: pickedOption?.displayDescription ?? String(localized: "Answer in your own words…")) {
                    send(answerTo: question)
                }
                Button {
                    contextNote = ""
                    askingContext = true
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
                    let text = reply.trimmingCharacters(in: .whitespacesAndNewlines)
                    resolve(Stamp(label: String(localized: "Sent"), symbol: "paperplane.fill", tint: Palette.sky), kind: .replyToReport(report.id)) { [attachments] client in
                        try await client.send(to: report.sessionId, text: text, attachments: try await attachments.storedIDs(using: client))
                        try await client.markRead(report.id)
                    }
                }
                if report.primaryAction() == .review {
                    Button {
                        review(report, .comments)
                    } label: {
                        Label("Rescue Comments", systemImage: "text.bubble")
                            .font(.headline)
                            .frame(maxWidth: .infinity, minHeight: 50)
                    }
                    .buttonStyle(FilledStyle(tint: actionTint))
                    HStack(spacing: 24) {
                        Button {
                            review(report, .approved)
                        } label: {
                            Label("Approve", systemImage: "checkmark.seal")
                                .font(.subheadline.weight(.medium))
                                .frame(minHeight: 30)
                        }
                        Button {
                            markRead(report)
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
                        markRead(report)
                    } label: {
                        Label("Mark as Read", systemImage: "checkmark")
                            .font(.headline)
                            .frame(maxWidth: .infinity, minHeight: 50)
                    }
                    .buttonStyle(FilledStyle(tint: actionTint))
                }
            }
        case .pullRequest(let pr):
            VStack(spacing: 10) {
                replyField(placeholder: String(localized: "What should change?")) {
                    let text = reply.trimmingCharacters(in: .whitespacesAndNewlines)
                    resolve(Stamp(label: String(localized: "Changes requested"), symbol: "arrow.uturn.backward", tint: Palette.dawn), kind: .requestChanges(pr.id)) { [attachments] client in
                        try await client.requestChanges(onPullRequest: pr.id, text: text, attachments: try await attachments.storedIDs(using: client))
                    }
                }
                Button {
                    ask(onPullRequest: pr)
                } label: {
                    Label("Ask", systemImage: "questionmark.bubble")
                        .font(.subheadline.weight(.medium))
                        .foregroundStyle(.secondary)
                        .frame(minHeight: 30)
                }
                .buttonStyle(.plain)
                .disabled((reply.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && attachments.isEmpty) || asking)
                PRMainAction(pr: pr, placement: .card) { settle($0, of: pr) }
                PRFooter(pr: pr) { settle(.setAside, of: pr) }
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
                        withAnimation(.spring(duration: 0.3)) { self.pickedOption = nil }
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
            AttachmentStrip(attachments, client: fleet.client)
            HStack(alignment: .bottom, spacing: 8) {
                AttachButton(attachments: attachments)
                    .padding(.leading, 6)
                    .padding(.bottom, 4)
                TextField(placeholder, text: $reply, axis: .vertical)
                    .lineLimit(1...5)
                    .focused($replyFocused)
                    .padding(.trailing, 16)
                    .padding(.vertical, 12)
                let empty = reply.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && attachments.isEmpty
                Button(action: onSend) {
                    Image(systemName: "arrow.up")
                        .font(.body.weight(.bold))
                        .foregroundStyle(Palette.onAccent)
                        .frame(width: 36, height: 36)
                        .background(empty ? Palette.slate.opacity(0.4) : kind.tint, in: .circle)
                }
                .buttonStyle(PressStyle())
                .disabled(empty)
                .padding(4)
                .accessibilityLabel("Send")
                .animation(.spring(duration: 0.3), value: empty)
            }
            .background(Palette.cardRaised, in: .rect(cornerRadius: 22, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 22, style: .continuous).strokeBorder(replyFocused ? kind.tint.opacity(0.5) : Palette.hairline))
        }
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

    private func choose(_ option: QuestionOption, in question: Question) {
        if Self.needsText(option) {
            withAnimation(.spring(duration: 0.35, bounce: 0.3)) { pickedOption = option }
            replyFocused = true
            return
        }
        resolve(stamp(for: option.label), kind: .answer(question.id)) { try await $0.answer(question.id, text: option.label, option: option.label) }
    }

    private func send(answerTo question: Question) {
        let text = reply.trimmingCharacters(in: .whitespacesAndNewlines)
        let option = pickedOption?.label
        resolve(option.map(stamp(for:)) ?? Stamp(label: String(localized: "Answered"), symbol: "paperplane.fill", tint: Palette.sky), kind: .answer(question.id)) { [attachments] client in
            try await client.answer(question.id, text: text, option: option, attachments: try await attachments.storedIDs(using: client))
        }
    }

    private func requestContext() {
        guard case .question(let question) = item else { return }
        let note = contextNote.trimmingCharacters(in: .whitespacesAndNewlines)
        resolve(Stamp(label: String(localized: "Context requested"), symbol: "questionmark", tint: Palette.slate), kind: .moreContext(question.id)) {
            try await $0.moreContext(question.id, note: note)
        }
    }

    private func markRead(_ report: Report) {
        resolve(stamp(for: HandoffLabel.read), kind: .markRead(report.id)) { try await $0.markRead(report.id) }
    }

    private func review(_ report: Report, _ verdict: ReviewVerdict) {
        let stamp = switch verdict {
        case .comments: Stamp(label: String(localized: "Comments sent"), symbol: "text.bubble.fill", tint: Palette.sky)
        case .approved: Stamp(label: String(localized: "Approved"), symbol: "checkmark.seal.fill", tint: actionTint)
        }
        resolve(stamp, kind: .review(report.id)) { try await $0.review(report.id, verdict) }
    }

    private func stamp(for label: String) -> Stamp {
        switch label {
        case HandoffLabel.read: return Stamp(label: String(localized: "Read"), symbol: "checkmark", tint: actionTint)
        case HandoffLabel.answered: return Stamp(label: String(localized: "Answered"), symbol: "paperplane.fill", tint: kind.tint)
        case HandoffLabel.sent: return Stamp(label: String(localized: "Sent"), symbol: "paperplane.fill", tint: kind.tint)
        case HandoffLabel.merged: return Stamp(label: String(localized: "Merged"), symbol: "arrow.triangle.merge", tint: Palette.sage)
        case HandoffLabel.conflictsSent: return Stamp(label: String(localized: "Sent to its node"), symbol: "arrow.triangle.branch", tint: Palette.dawn)
        case HandoffLabel.setAside: return Stamp(label: String(localized: "Set aside"), symbol: "tray.and.arrow.down", tint: Palette.slate)
        default: break
        }
        let shown = AnswerOption.displayLabel(label)
        return switch OptionTone(label) {
        case .positive: Stamp(label: shown, symbol: "checkmark", tint: Palette.sage)
        case .negative: Stamp(label: shown, symbol: "xmark", tint: Palette.rose)
        case .neutral: Stamp(label: shown, symbol: "paperplane.fill", tint: kind.tint)
        }
    }

    /// Unlike `resolve`, the PR's card stays live on the server (asking never parks it), so
    /// this must not hide it locally: it just sends and clears the draft.
    private func ask(onPullRequest pr: PullRequest) {
        guard !asking else { return }
        asking = true
        replyFocused = false
        let text = reply.trimmingCharacters(in: .whitespacesAndNewlines)
        Task {
            if await fleet.ask(onPullRequest: pr.id, text: text, attachments: attachments) {
                reply = ""
                attachments.removeAll()
            }
            asking = false
        }
    }

    private func settle(_ outcome: PullRequestOutcome, of pr: PullRequest) {
        resolve(stamp(for: outcome.handoffLabel), kind: outcome.commandKind(for: pr.id)) { try await pr.perform(outcome, using: $0) }
    }

    private func resolve(_ stamp: Stamp, kind: FleetStore.MutationKind, _ action: @escaping @Sendable (AlondraClient) async throws -> Void) {
        guard self.stamp == nil else { return }
        replyFocused = false
        withAnimation(.spring(duration: 0.35, bounce: 0.4)) { self.stamp = stamp }
        fleet.leaving = item.id
        let item = item
        let fleet = fleet
        Task {
            try? await Task.sleep(for: .milliseconds(520))
            let ok = await fleet.resolve(item, kind: kind, action)
            if !ok {
                withAnimation(.spring(duration: 0.3)) { self.stamp = nil }
            }
        }
    }

    static func needsText(_ option: QuestionOption) -> Bool {
        if let description = option.description, ["write", "escribe"].contains(where: { description.lowercased().hasPrefix($0) }) { return true }
        return [AnswerOption.requestChanges, AnswerOption.deny].contains(option.label)
    }
}

/// Outcomes handed over from ReviewView and PRReviewView that are not answer options.
enum HandoffLabel {
    static let read = "alondra.read"
    static let answered = "alondra.answered"
    static let sent = "alondra.sent"
    static let merged = "alondra.merged"
    static let conflictsSent = "alondra.conflictsSent"
    static let setAside = "alondra.setAside"
}

enum OptionTone {
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

struct OptionGrid: View {
    let options: [QuestionOption]
    let picked: QuestionOption?
    let onPick: (QuestionOption) -> Void

    @Environment(\.dynamicTypeSize) private var typeSize

    private var compact: Bool {
        options.count == 2 && options.allSatisfy { $0.displayLabel.count <= 16 && (TriageCard.needsText($0) || $0.description == nil) } && !typeSize.isAccessibilitySize
    }

    var body: some View {
        let layout = compact ? AnyLayout(HStackLayout(spacing: 10)) : AnyLayout(VStackLayout(spacing: 8))
        layout {
            ForEach(options, id: \.label) { option in
                Button { onPick(option) } label: {
                    VStack(alignment: compact ? .center : .leading, spacing: 2) {
                        Text(option.displayLabel)
                            .font(.headline)
                        if let description = option.displayDescription, !compact, !TriageCard.needsText(option) {
                            Text(description)
                                .font(.caption)
                                .opacity(0.8)
                                .multilineTextAlignment(.leading)
                        }
                    }
                    .frame(maxWidth: .infinity, minHeight: 46, alignment: compact ? .center : .leading)
                    .padding(.horizontal, compact ? 0 : 6)
                }
                .buttonStyle(OptionStyle(tone: OptionTone(option.label), picked: picked == option))
            }
        }
    }
}

private struct OptionStyle: ButtonStyle {
    let tone: OptionTone
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

struct FilledStyle: ButtonStyle {
    let tint: Color

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .foregroundStyle(Palette.onAccent)
            .background(tint, in: .rect(cornerRadius: 18, style: .continuous))
            .scaleEffect(configuration.isPressed ? 0.97 : 1)
            .animation(.spring(duration: 0.25, bounce: 0.4), value: configuration.isPressed)
    }
}

struct PressStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .scaleEffect(configuration.isPressed ? 0.96 : 1)
            .opacity(configuration.isPressed ? 0.85 : 1)
            .animation(.spring(duration: 0.25, bounce: 0.4), value: configuration.isPressed)
    }
}
