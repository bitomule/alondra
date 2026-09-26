import AlondraAttachmentsFeature
import AlondraWorkspaceFeature
import AlondraQuestionsFeature
import AlondraShared
import AlondraTriageFeature
import ComposableArchitecture
import SwiftUI

struct ServiceTriageView: View {
    @Bindable var store: StoreOf<ServiceTriageFeature>
    @Namespace private var zoom
    @State private var showMachine = false
    @State private var keyboardVisible = false
    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if !keyboardVisible { header.padding(.horizontal, 20).transition(.move(edge: .top).combined(with: .opacity)) }
            if !store.triage.hasSnapshot && store.triage.queue.isEmpty {
                VStack(spacing: 16) { ProgressView().controlSize(.large).tint(Palette.dawn); Text("Connecting to the fleet…").font(.subheadline).foregroundStyle(.secondary) }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if store.triage.queue.isEmpty {
                ServiceEmptyTriage(nodes: store.nodes).transition(.opacity.combined(with: .scale(scale: 0.96)))
            } else {
                ServiceCardStack(store: store, zoom: zoom) { store.send(.open($0.id)) }
                    .padding(.horizontal, 14).padding(.bottom, 8).transition(.opacity)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .background(DawnBackground())
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarLeading) {
                Button { showMachine = true } label: { ServiceMachineBadge(machine: store.machine, usage: store.usage, online: store.triage.hasSnapshot) }
                    .popover(isPresented: $showMachine) { ServiceMachineDetail(machine: store.machine, usage: store.usage, online: store.triage.hasSnapshot).presentationCompactAdaptation(.popover) }
                    .accessibilityLabel("Mac Status")
            }
        }
        .alert("Check Alondra", isPresented: Binding(get: { store.triage.notice != nil }, set: { if !$0 { store.send(.triage(.dismissNotice)) } })) {
            Button("OK") { store.send(.triage(.dismissNotice)) }
        } message: { Text(store.triage.notice ?? "") }
        .navigationDestination(item: $store.scope(state: \.question, action: \.question)) { child in
            ServiceQuestionView(store: child).navigationTransition(.zoom(sourceID: "q\(child.question.id)", in: zoom))
        }
        .navigationDestination(item: $store.scope(state: \.document, action: \.document)) { child in
            ServiceDocumentView(store: child, markRead: { store.send(.triage(.hide(child.subject.id))); store.send(.document(.dismiss)) })
                .navigationTransition(.zoom(sourceID: child.subject.id, in: zoom))
        }
        .onReceive(NotificationCenter.default.publisher(for: UIResponder.keyboardWillShowNotification)) { _ in withAnimation(.smooth(duration: 0.3)) { keyboardVisible = true } }
        .onReceive(NotificationCenter.default.publisher(for: UIResponder.keyboardWillHideNotification)) { _ in withAnimation(.smooth(duration: 0.3)) { keyboardVisible = false } }
        .sensoryFeedback(.success, trigger: store.triage.resolvedCount)
        .sensoryFeedback(.impact(weight: .light), trigger: store.triage.skippedCount)
        .task { await store.send(.triage(.appeared)).finish() }
        .onDisappear { store.send(.triage(.disappeared)) }
    }
    private var header: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Waiting for You")
                .font(.display(.largeTitle, weight: .bold))
                .foregroundStyle(Palette.ink)
            HStack(spacing: 8) {
                Text(summary).contentTransition(.numericText())
            }
            .font(.subheadline.weight(.medium))
            .foregroundStyle(.secondary)
            .lineLimit(1)
            if store.triage.queue.count > 1 {
                ServicePageDots(count: store.triage.queue.count, current: store.triage.position)
                    .padding(.top, 6)
            }
        }
        .padding(.bottom, 14)
    }

    private var summary: String {
        let queue = store.triage.queue
        let questions = queue.filter { if case .question = $0 { true } else { false } }.count
        let reads = queue.count - questions
        var parts: [String] = []
        if questions > 0 { parts.append(String(localized: "\(questions) questions")) }
        if reads > 0 { parts.append(String(localized: "\(reads) to read")) }
        let working = store.triage.workingNodes
        parts.append(String(localized: "\(working) nodes working"))
        return parts.joined(separator: " · ")
    }

}

private struct ServiceCardStack: View {
    let store: StoreOf<ServiceTriageFeature>
    let zoom: Namespace.ID
    let onReview: (InboxItem) -> Void

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var drag: CGFloat = 0
    @State private var frontHeight: CGFloat?

    private let depthLimit = 3

    var body: some View {
        let queue = store.triage.queue
        let visible = Array(queue.prefix(depthLimit))
        VStack(spacing: 10) {
            ZStack(alignment: .top) {
                ForEach(visible.reversed()) { item in
                    let depth = visible.firstIndex(of: item) ?? 0
                    card(item, isFront: depth == 0)
                        .frame(maxHeight: depth == 0 ? nil : frontHeight, alignment: .top)
                        .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { height in
                            if depth == 0 { frontHeight = height }
                        }
                        .scaleEffect(1 - CGFloat(depth) * 0.05, anchor: .top)
                        .offset(y: CGFloat(depth) * -11)
                        .brightness(depth == 0 ? 0 : -0.04 * Double(depth))
                        .offset(x: depth == 0 ? drag : 0)
                        .opacity(depth == 0 ? 1 - Double(min(abs(drag) / 900, 0.35)) : 1)
                        .zIndex(store.triage.hiddenIDs.contains(item.id) ? 10 : Double(depthLimit - depth))
                        .allowsHitTesting(depth == 0)
                        .accessibilityHidden(depth != 0)
                        .transition(transition(for: item))
                }
            }
            .padding(.top, CGFloat(min(visible.count, depthLimit) - 1) * 11)
            .simultaneousGesture(swipe(visible.first, count: queue.count))
            .animation(.spring(duration: 0.5, bounce: 0.22), value: visible.map(\.id))

        }
    }

    @ViewBuilder
    private func card(_ item: InboxItem, isFront: Bool) -> some View {
        if let child = store.scope(state: \.cards, action: \.cards).first(where: { $0.id == item.id }) {
            ServiceTriageCard(store: child, node: store.nodes.first { $0.sessionId == item.sessionId }, task: store.tasks.first { $0.assignee?.sessionId == item.sessionId && !$0.isArchived },
                isFront: isFront, zoom: zoom, onReview: { onReview(item) }, onSkip: { skip(item) },
                onHide: { store.send(.triage(.hide(item.id))) },
                questions: { if case .pullRequest(let pr) = item { return store.triage.questionsByPullRequest[pr.id] ?? [] }; return [] }(),
                reports: { if case .pullRequest(let pr) = item { return store.triage.reportsByPullRequest[pr.id] ?? [] }; return [] }())
        }
    }

    private func transition(for item: InboxItem) -> AnyTransition {
        if reduceMotion { return .opacity }
        let removal: AnyTransition = store.triage.hiddenIDs.contains(item.id) ? AnyTransition(FileAway()) : .opacity
        return .asymmetric(insertion: .opacity.combined(with: .scale(scale: 0.96, anchor: .top)), removal: removal)
    }

    private func swipe(_ front: InboxItem?, count: Int) -> some Gesture {
        DragGesture(minimumDistance: 24)
            .onChanged { value in
                guard front != nil, count > 1, abs(value.translation.width) > abs(value.translation.height) * 1.4 else { return }
                drag = value.translation.width
            }
            .onEnded { value in
                guard let front, count > 1 else { return }
                let projected = value.predictedEndTranslation.width
                let committed = abs(drag) > 90 || (abs(projected) > 240 && abs(drag) > 30)
                guard committed else {
                    withAnimation(.spring(duration: 0.4, bounce: 0.35)) { drag = 0 }
                    return
                }
                next(front)
            }
    }

    /// Swiping either way moves on to the next card; the card leaves in the direction it was pushed.
    private func next(_ item: InboxItem) {
        let away: CGFloat = drag > 0 ? 420 : -420
        let identity = store.triage.identity
        withAnimation(.easeIn(duration: 0.16)) { drag = away }
        withAnimation(.easeIn(duration: 0.16), completionCriteria: .logicallyComplete) {
            drag = away
        } completion: {
            var instant = Transaction(); instant.disablesAnimations = true
            withTransaction(instant) { drag = 0 }
            withAnimation(.spring(duration: 0.5, bounce: 0.2)) { store.send(.triage(.skipIfCurrent(item.id, identity))) }
        }
    }

    private func skip(_ item: InboxItem) {
        next(item)
    }
}

private struct FileAway: Transition {
    func body(content: Content, phase: TransitionPhase) -> some View {
        content
            .scaleEffect(phase.isIdentity ? 1 : 0.86, anchor: .top)
            .offset(y: phase.isIdentity ? 0 : -120)
            .rotation3DEffect(.degrees(phase.isIdentity ? 0 : 14), axis: (x: 1, y: 0, z: 0), anchor: .top)
            .opacity(phase.isIdentity ? 1 : 0)
            .blur(radius: phase.isIdentity ? 0 : 8)
    }
}

private struct ServicePageDots: View {
    let count: Int
    let current: Int

    var body: some View {
        Group {
            if count <= 8 {
                HStack(spacing: 6) {
                    ForEach(0..<count, id: \.self) { index in
                        Capsule()
                            .fill(index == current ? Palette.dawn : Palette.slate.opacity(0.35))
                            .frame(width: index == current ? 18 : 6, height: 6)
                    }
                }
            } else {
                Text("\(current + 1) of \(count)")
                    .font(.caption.weight(.semibold).monospacedDigit())
                    .foregroundStyle(.secondary)
                    .contentTransition(.numericText())
            }
        }
        .animation(.spring(duration: 0.35, bounce: 0.2), value: current)
        .animation(.spring(duration: 0.4), value: count)
        .accessibilityElement()
        .accessibilityLabel("\(current + 1) of \(count) pending")
    }
}

private struct ServiceEmptyTriage: View {
    let nodes: [Node]

    var body: some View {
        VStack(spacing: 18) {
            Spacer()
            Image(systemName: "sunrise.fill")
                .font(.system(size: 64, weight: .light))
                .foregroundStyle(LinearGradient(colors: [Palette.dawn, Palette.ember], startPoint: .top, endPoint: .bottom))
                .symbolEffect(.breathe.pulse, options: .repeat(.continuous))
            Text("Nothing is waiting for you")
                .font(.display(.title, weight: .bold))
            Text("The fleet carries on by itself. When a node needs you, it will show up here.")
                .font(.body)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 300)
            HStack(spacing: 10) {
                ForEach(stats, id: \.label) { stat in
                    VStack(spacing: 2) {
                        Text(stat.count, format: .number)
                            .font(.display(.title2, weight: .bold))
                            .foregroundStyle(stat.tint)
                            .contentTransition(.numericText())
                        Text(stat.label)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    .frame(minWidth: 88)
                    .padding(.vertical, 12)
                    .background(Palette.card.opacity(0.7), in: .rect(cornerRadius: 18))
                }
            }
            .padding(.top, 8)
            Spacer()
            Spacer()
        }
        .padding(.horizontal, 24)
        .frame(maxWidth: .infinity)
    }

    private var stats: [(label: String, count: Int, tint: Color)] {
        return [
            (String(localized: "working"), nodes.filter { $0.state == .working }.count, Palette.sky),
            (String(localized: "waiting"), nodes.filter { $0.state == .waitingAnswer || $0.state == .yourTurn || $0.state == .waitingMerge || $0.state == .waitingMachine }.count, Palette.dawn),
            (String(localized: "total"), nodes.count, Palette.sage),
        ]
    }
}

struct ServiceMachineBadge: View {
    let machine: Machine?
    let usage: Usage?
    let online: Bool

    var body: some View {
        HStack(spacing: 6) {
            Circle()
                .fill(dotColor)
                .frame(width: 8, height: 8)
                .shadow(color: dotColor.opacity(0.8), radius: 4)
            Image(systemName: "desktopcomputer")
                .font(.footnote)
            if let machine = machine {
                ForEach(StatusReadings.of(machine: machine, usage: usage)) { reading in
                    HStack(spacing: 3) {
                        ReadingBar(value: reading.value, tint: reading.level.tint)
                        Text(verbatim: reading.label)
                            .font(.caption2.monospacedDigit().weight(.medium))
                            .foregroundStyle(reading.level == .hot ? Palette.rose : Palette.ink.opacity(0.65))
                            .contentTransition(.numericText())
                    }
                }
            }
        }
        .foregroundStyle(Palette.ink.opacity(0.65))
        .animation(.smooth, value: online)
    }

    private var dotColor: Color {
        online ? (machine?.unreliable == true ? Palette.dawn : Palette.sage) : Palette.rose
    }
}

private struct ReadingBar: View {
    let value: Double
    let tint: Color

    var body: some View {
        Capsule()
            .fill(Palette.ink.opacity(0.15))
            .frame(width: 22, height: 4)
            .overlay(alignment: .leading) {
                Capsule()
                    .fill(tint)
                    .frame(width: 22 * max(0.06, min(value, 1)))
            }
    }
}

private extension StatusReading.Level {
    var tint: Color {
        switch self {
        case .calm: Palette.sky
        case .warm: Palette.dawn
        case .hot: Palette.rose
        }
    }
}

private struct ServiceMachineDetail: View {
    let machine: Machine?
    let usage: Usage?
    let online: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Mac")
                .font(.display(.title3, weight: .bold))
            if online {
                if let machine = machine {
                    ForEach(StatusReadings.of(machine: machine, usage: usage)) { reading in
                        HStack(alignment: .firstTextBaseline, spacing: 10) {
                            ReadingBar(value: reading.value, tint: reading.level.tint)
                            Text(reading.help)
                                .foregroundStyle(reading.level == .hot ? Palette.rose : Palette.ink)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                    }
                    row(String(localized: "Free disk"), ByteCountFormatter.string(fromByteCount: Int64(machine.diskAvailable), countStyle: .file))
                }
            }
        }
        .font(.subheadline)
        .padding(20)
        .frame(minWidth: 260, alignment: .leading)
    }

    private func row(_ label: String, _ value: String) -> some View {
        HStack {
            Text(label).foregroundStyle(.secondary)
            Spacer(minLength: 24)
            Text(value).monospacedDigit().fontWeight(.medium)
        }
    }
}


struct ServiceQuestionView: View {
    let store: StoreOf<QuestionFeature>
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        Form {
            Section {
                Text(store.question.title).font(.headline)
                if let detail = store.question.readableDetail { Text(detail).textSelection(.enabled) }
            }
            ForEach(store.question.aboutReports) { report in
                Section(report.title) { ServiceReportContext(report: report) }
            }
            Section("Answer") {
                ForEach(store.question.options, id: \.label) { option in
                    Button {
                        store.send(.optionSelected(option.label))
                    } label: {
                        HStack {
                            VStack(alignment: .leading) {
                                Text(option.label)
                                if let detail = option.description { Text(detail).font(.caption).foregroundStyle(.secondary) }
                            }
                            Spacer()
                            if store.draft.selectedOption == option.label { Image(systemName: "checkmark") }
                        }
                    }
                }
                if !store.question.isTerminalMenu {
                    TextField("Write your answer", text: Binding(get: { store.draft.text }, set: { store.send(.textChanged($0)) }), axis: .vertical)
                        .lineLimit(4...12)
                }
                Button(store.isSubmitting ? "Sending…" : "Send answer") { store.send(.answerTapped) }
                if !store.question.isTerminalMenu {
                    Button("Ask for more context") { store.send(.moreContextTapped) }
                }
            }.disabled(!store.attachmentPicker.allReady || store.isSubmitting || store.needsReconciliation || store.completed)
            AttachmentPicker(store: store.scope(state: \.attachmentPicker, action: \.attachmentPicker))
                .disabled(store.isSubmitting || store.needsReconciliation || store.completed)
            ServiceWorkNotice(notice: store.notice, conflict: store.conflict)
        }
        .navigationTitle(store.question.isAction ? "Action" : "Question")
        .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Close") { dismiss() } } }
    }
}

private struct ServiceWorkNotice: View {
    let notice: String?
    let conflict: String?
    var body: some View {
        Section {
            if let conflict { Text(conflict).foregroundStyle(.orange) }
            if let notice { Text(notice).foregroundStyle(.red) }
        }
    }
}

private struct ServiceReportContext: View {
    let report: Report
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if let body = report.body {
                ForEach(MarkdownParser.segments(from: body)) { segment in
                    MarkdownSegmentView(segment: segment)
                }
            }
        }.textSelection(.enabled)
    }
}
