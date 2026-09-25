import AlondraShared
import SwiftUI

struct TriageView: View {
    @Environment(FleetStore.self) private var fleet
    @Environment(PushCenter.self) private var push
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Namespace private var zoom
    @State private var reviewing: InboxItem?
    @State private var showSettings = false
    @State private var showPairing = false
    @State private var showMachine = false
    @State private var keyboardVisible = false

    var body: some View {
        NavigationStack {
            VStack(alignment: .leading, spacing: 0) {
                if !keyboardVisible {
                    header
                        .padding(.horizontal, 20)
                        .transition(.move(edge: .top).combined(with: .opacity))
                }
                if fleet.connection == .unpaired {
                    UnpairedNotice { showPairing = true }
                        .transition(.opacity.combined(with: .scale(scale: 0.96)))
                } else if fleet.overview == nil {
                    loading
                } else if fleet.queue.isEmpty {
                    EmptyTriage()
                        .transition(.opacity.combined(with: .scale(scale: 0.96)))
                } else {
                    CardStack(zoom: zoom) { reviewing = $0 }
                        .padding(.horizontal, 14)
                        .padding(.bottom, 8)
                        .transition(.opacity)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            .background(DawnBackground())
            .toolbar { toolbar }
            .navigationBarTitleDisplayMode(.inline)
            .navigationDestination(item: $reviewing) { item in
                Group {
                    if case .pullRequest(let pr) = item {
                        PRReviewView(pr: pr)
                    } else {
                        ReviewView(item: item)
                    }
                }
                .navigationTransition(.zoom(sourceID: item.id, in: zoom))
            }
            .sheet(isPresented: $showSettings) { SettingsView() }
            .sheet(isPresented: $showPairing) {
                NavigationStack { PairingView(showsClose: true) }
            }
        }
        .onReceive(NotificationCenter.default.publisher(for: UIResponder.keyboardWillShowNotification)) { _ in
            withAnimation(.smooth(duration: 0.3)) { keyboardVisible = true }
        }
        .onReceive(NotificationCenter.default.publisher(for: UIResponder.keyboardWillHideNotification)) { _ in
            withAnimation(.smooth(duration: 0.3)) { keyboardVisible = false }
        }
        .sensoryFeedback(.success, trigger: fleet.resolvedCount)
        .sensoryFeedback(.impact(weight: .light), trigger: fleet.skippedCount)
        .task(id: push.route) {
            guard let route = push.route, push.isCurrent(route) else { push.route = nil; return }
            await fleet.refresh()
            open(route, giveUp: fleet.connected)
        }
        .onChange(of: fleet.inbox) { open(push.route, giveUp: false) }
    }

    private func open(_ route: NotificationRoute?, giveUp: Bool) {
        guard let route else { return }
        guard push.isCurrent(route) else { push.route = nil; return }
        guard let item = fleet.inbox.first(where: { $0.id == route.itemID }) else {
            if giveUp {
                push.route = nil
                fleet.notice = Notice(String(localized: "That’s no longer pending"))
            }
            return
        }
        push.route = nil
        showSettings = false
        showPairing = false
        reviewing = nil
        fleet.focus(item)
        if route.opensDetail { reviewing = item }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Waiting for You")
                .font(.display(.largeTitle, weight: .bold))
                .foregroundStyle(Palette.ink)
            HStack(spacing: 8) {
                if case .offline(let reason) = fleet.connection {
                    Label(reason, systemImage: "wifi.slash")
                        .foregroundStyle(Palette.rose)
                } else if fleet.connection == .unpaired {
                    Label("Not paired", systemImage: "lock.slash")
                        .foregroundStyle(Palette.rose)
                } else {
                    Text(summary)
                        .contentTransition(.numericText())
                }
            }
            .font(.subheadline.weight(.medium))
            .foregroundStyle(.secondary)
            .lineLimit(1)
            if fleet.queue.count > 1 {
                PageDots(count: fleet.queue.count, current: fleet.position % fleet.queue.count)
                    .padding(.top, 6)
            }
        }
        .padding(.bottom, 14)
        .animation(.smooth, value: fleet.connection)
    }

    private var summary: String {
        let queue = fleet.queue
        let questions = queue.filter { if case .question = $0 { true } else { false } }.count
        let reads = queue.count - questions
        var parts: [String] = []
        if questions > 0 { parts.append(String(localized: "\(questions) questions")) }
        if reads > 0 { parts.append(String(localized: "\(reads) to read")) }
        let working = fleet.nodes.filter { $0.state == .working }.count
        parts.append(String(localized: "\(working) nodes working"))
        return parts.joined(separator: " · ")
    }

    private var loading: some View {
        VStack(spacing: 16) {
            ProgressView()
                .controlSize(.large)
                .tint(Palette.dawn)
            Text(fleet.connection == .connecting ? String(localized: "Connecting to the fleet…") : String(localized: "Retrying every few seconds…"))
                .font(.subheadline)
                .foregroundStyle(.secondary)
            if case .offline = fleet.connection {
                Button("Check the Server", systemImage: "gearshape") { showSettings = true }
                    .buttonStyle(.glass)
                    .padding(.top, 8)
                    .transition(.opacity.combined(with: .scale(scale: 0.9)))
            }
        }
        .animation(.smooth, value: fleet.connection)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    @ToolbarContentBuilder
    private var toolbar: some ToolbarContent {
        ToolbarItem(placement: .topBarLeading) {
            Button { showMachine = true } label: {
                MachineBadge()
            }
            .popover(isPresented: $showMachine) {
                MachineDetail()
                    .presentationCompactAdaptation(.popover)
            }
            .accessibilityLabel("Mac Status")
        }
        ToolbarItem(placement: .topBarTrailing) {
            Button("Settings", systemImage: "gearshape") { showSettings = true }
        }
    }
}

private struct CardStack: View {
    let zoom: Namespace.ID
    let onReview: (InboxItem) -> Void

    @Environment(FleetStore.self) private var fleet
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var drag: CGFloat = 0
    @State private var frontHeight: CGFloat?

    private let depthLimit = 3

    var body: some View {
        let queue = fleet.queue
        let visible = Array(queue.prefix(depthLimit))
        VStack(spacing: 10) {
            ZStack(alignment: .top) {
                ForEach(visible.reversed()) { item in
                    let depth = visible.firstIndex(of: item) ?? 0
                    TriageCard(item: item, isFront: depth == 0, zoom: zoom, onReview: { onReview(item) }, onSkip: { skip(item) })
                        .frame(maxHeight: depth == 0 ? nil : frontHeight, alignment: .top)
                        .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { height in
                            if depth == 0 { frontHeight = height }
                        }
                        .scaleEffect(1 - CGFloat(depth) * 0.05, anchor: .top)
                        .offset(y: CGFloat(depth) * -11)
                        .brightness(depth == 0 ? 0 : -0.04 * Double(depth))
                        .offset(x: depth == 0 ? drag : 0)
                        .opacity(depth == 0 ? 1 - Double(min(abs(drag) / 900, 0.35)) : 1)
                        .zIndex(item.id == fleet.leaving ? 10 : Double(depthLimit - depth))
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

    private func transition(for item: InboxItem) -> AnyTransition {
        if reduceMotion { return .opacity }
        let removal: AnyTransition = item.id == fleet.leaving ? AnyTransition(FileAway()) : .opacity
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
        withAnimation(.easeIn(duration: 0.16)) { drag = away }
        Task {
            try? await Task.sleep(for: .milliseconds(160))
            var instant = Transaction()
            instant.disablesAnimations = true
            withTransaction(instant) { drag = 0 }
            withAnimation(.spring(duration: 0.5, bounce: 0.2)) { fleet.skip(item) }
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

private struct PageDots: View {
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

private struct EmptyTriage: View {
    @Environment(FleetStore.self) private var fleet

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
        let nodes = fleet.nodes
        return [
            (String(localized: "working"), nodes.filter { $0.state == .working }.count, Palette.sky),
            (String(localized: "waiting"), nodes.filter { $0.state == .waitingAnswer || $0.state == .yourTurn || $0.state == .waitingMerge || $0.state == .waitingMachine }.count, Palette.dawn),
            (String(localized: "total"), nodes.count, Palette.sage),
        ]
    }
}

struct MachineBadge: View {
    @Environment(FleetStore.self) private var fleet

    var body: some View {
        HStack(spacing: 6) {
            Circle()
                .fill(dotColor)
                .frame(width: 8, height: 8)
                .shadow(color: dotColor.opacity(0.8), radius: 4)
            Image(systemName: "desktopcomputer")
                .font(.footnote)
            if let machine = fleet.machine {
                ForEach(StatusReadings.of(machine: machine, usage: fleet.usage)) { reading in
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
        .animation(.smooth, value: fleet.connection)
    }

    private var dotColor: Color {
        switch fleet.connection {
        case .online: fleet.machine?.unreliable == true ? Palette.dawn : Palette.sage
        case .connecting: Palette.slate
        case .offline, .unpaired: Palette.rose
        }
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

private struct MachineDetail: View {
    @Environment(FleetStore.self) private var fleet

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Mac")
                .font(.display(.title3, weight: .bold))
            switch fleet.connection {
            case .offline(let reason):
                Label(reason, systemImage: "wifi.slash").foregroundStyle(Palette.rose)
            case .connecting:
                Label("Connecting…", systemImage: "antenna.radiowaves.left.and.right")
            case .unpaired:
                Label("This device is not paired", systemImage: "lock.slash").foregroundStyle(Palette.rose)
            case .online:
                if let machine = fleet.machine {
                    ForEach(StatusReadings.of(machine: machine, usage: fleet.usage)) { reading in
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

private struct UnpairedNotice: View {
    let pair: () -> Void

    var body: some View {
        VStack(spacing: 18) {
            Spacer()
            Image(systemName: "lock.iphone")
                .font(.system(size: 54, weight: .light))
                .foregroundStyle(Palette.rose)
                .symbolEffect(.pulse, options: .repeating)
            VStack(spacing: 8) {
                Text("This device is not paired")
                    .font(.display(.title2, weight: .bold))
                    .foregroundStyle(Palette.ink)
                    .multilineTextAlignment(.center)
                Text("The Mac no longer accepts this iPhone. Pair it again with the code shown by alondra pair.")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
            }
            Button("Pair Again", systemImage: "qrcode.viewfinder", action: pair)
                .buttonStyle(.glassProminent)
                .controlSize(.large)
                .padding(.top, 6)
            Spacer()
            Spacer()
        }
        .padding(.horizontal, 32)
        .frame(maxWidth: .infinity)
    }
}
