import AlondraShared
import SwiftUI

struct TriageView: View {
    @Environment(Fleet.self) private var fleet
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Namespace private var zoom
    @State private var reviewing: InboxItem?
    @State private var showSettings = false
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
                if fleet.overview == nil {
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
                ReviewView(item: item)
                    .navigationTransition(.zoom(sourceID: item.id, in: zoom))
            }
            .sheet(isPresented: $showSettings) { SettingsView() }
        }
        .onReceive(NotificationCenter.default.publisher(for: UIResponder.keyboardWillShowNotification)) { _ in
            withAnimation(.smooth(duration: 0.3)) { keyboardVisible = true }
        }
        .onReceive(NotificationCenter.default.publisher(for: UIResponder.keyboardWillHideNotification)) { _ in
            withAnimation(.smooth(duration: 0.3)) { keyboardVisible = false }
        }
        .sensoryFeedback(.success, trigger: fleet.resolvedCount)
        .sensoryFeedback(.impact(weight: .light), trigger: fleet.skippedCount)
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Te espera")
                .font(.display(.largeTitle, weight: .bold))
                .foregroundStyle(Palette.ink)
            HStack(spacing: 8) {
                if case .offline(let reason) = fleet.connection {
                    Label(reason, systemImage: "wifi.slash")
                        .foregroundStyle(Palette.rose)
                } else {
                    Text(summary)
                        .contentTransition(.numericText())
                }
            }
            .font(.subheadline.weight(.medium))
            .foregroundStyle(.secondary)
            .lineLimit(1)
        }
        .padding(.bottom, 14)
        .animation(.smooth, value: fleet.connection)
    }

    private var summary: String {
        let queue = fleet.queue
        let questions = queue.filter { if case .question = $0 { true } else { false } }.count
        let reads = queue.count - questions
        var parts: [String] = []
        if questions > 0 { parts.append(questions == 1 ? "1 pregunta" : "\(questions) preguntas") }
        if reads > 0 { parts.append(reads == 1 ? "1 para leer" : "\(reads) para leer") }
        let working = fleet.nodes.filter { $0.alive && $0.state == .working }.count
        parts.append(working == 1 ? "1 nodo trabajando" : "\(working) nodos trabajando")
        return parts.joined(separator: " · ")
    }

    private var loading: some View {
        VStack(spacing: 16) {
            ProgressView()
                .controlSize(.large)
                .tint(Palette.dawn)
            Text(fleet.connection == .connecting ? "Conectando con la flota…" : "Reintentando cada pocos segundos…")
                .font(.subheadline)
                .foregroundStyle(.secondary)
            if case .offline = fleet.connection {
                Button("Revisar el servidor", systemImage: "gearshape") { showSettings = true }
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
            .accessibilityLabel("Estado del Mac")
        }
        ToolbarItem(placement: .topBarTrailing) {
            Button("Ajustes", systemImage: "gearshape") { showSettings = true }
        }
    }
}

private struct CardStack: View {
    let zoom: Namespace.ID
    let onReview: (InboxItem) -> Void

    @Environment(Fleet.self) private var fleet
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
                        .frame(height: depth == 0 ? nil : frontHeight, alignment: .top)
                        .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { height in
                            if depth == 0 { frontHeight = height }
                        }
                        .scaleEffect(1 - CGFloat(depth) * 0.05, anchor: .top)
                        .offset(y: CGFloat(depth) * -11)
                        .brightness(depth == 0 ? 0 : -0.04 * Double(depth))
                        .offset(x: depth == 0 ? drag : 0)
                        .rotationEffect(.degrees(depth == 0 ? Double(drag) / 28 : 0), anchor: .bottom)
                        .zIndex(item.id == fleet.leaving ? 10 : Double(depthLimit - depth))
                        .allowsHitTesting(depth == 0)
                        .accessibilityHidden(depth != 0)
                        .transition(reduceMotion ? AnyTransition.opacity : .asymmetric(insertion: .opacity.combined(with: .scale(scale: 0.92, anchor: .top)), removal: AnyTransition(FileAway())))
                }
            }
            .padding(.top, CGFloat(min(visible.count, depthLimit) - 1) * 11)
            .simultaneousGesture(swipe(visible.first))
            .animation(.spring(duration: 0.5, bounce: 0.22), value: visible.map(\.id))

            if queue.count > 1 {
                PageDots(count: queue.count)
            }
        }
    }

    private func swipe(_ front: InboxItem?) -> some Gesture {
        DragGesture(minimumDistance: 24)
            .onChanged { value in
                guard front != nil, abs(value.translation.width) > abs(value.translation.height) * 1.4 else { return }
                drag = value.translation.width
            }
            .onEnded { value in
                guard let front else { return }
                let projected = value.predictedEndTranslation.width
                if abs(drag) > 110 || (abs(projected) > 260 && abs(drag) > 30) {
                    withAnimation(.easeIn(duration: 0.18)) { drag = drag > 0 ? 600 : -600 }
                    Task {
                        try? await Task.sleep(for: .milliseconds(180))
                        withAnimation(.spring(duration: 0.55, bounce: 0.2)) {
                            drag = 0
                            fleet.skip(front)
                        }
                    }
                } else {
                    withAnimation(.spring(duration: 0.4, bounce: 0.35)) { drag = 0 }
                }
            }
    }

    private func skip(_ item: InboxItem) {
        withAnimation(.easeIn(duration: 0.2)) { drag = -600 }
        Task {
            try? await Task.sleep(for: .milliseconds(200))
            withAnimation(.spring(duration: 0.55, bounce: 0.2)) {
                drag = 0
                fleet.skip(item)
            }
        }
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

    var body: some View {
        HStack(spacing: 6) {
            Capsule().fill(Palette.dawn).frame(width: 18, height: 6)
            ForEach(1..<min(count, 8), id: \.self) { _ in
                Circle().fill(Palette.slate.opacity(0.35)).frame(width: 6, height: 6)
            }
            if count > 8 {
                Text("+\(count - 8)")
                    .font(.caption2.weight(.semibold))
                    .foregroundStyle(.secondary)
            }
        }
        .animation(.spring(duration: 0.4), value: count)
        .accessibilityElement()
        .accessibilityLabel("\(count) cosas pendientes")
    }
}

private struct EmptyTriage: View {
    @Environment(Fleet.self) private var fleet

    var body: some View {
        VStack(spacing: 18) {
            Spacer()
            Image(systemName: "sunrise.fill")
                .font(.system(size: 64, weight: .light))
                .foregroundStyle(LinearGradient(colors: [Palette.dawn, Palette.ember], startPoint: .top, endPoint: .bottom))
                .symbolEffect(.breathe.pulse, options: .repeat(.continuous))
            Text("Nada te espera")
                .font(.display(.title, weight: .bold))
            Text("La flota sigue sola. Cuando un nodo te necesite, aparecerá aquí.")
                .font(.body)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 300)
            HStack(spacing: 10) {
                ForEach(stats, id: \.label) { stat in
                    VStack(spacing: 2) {
                        Text("\(stat.count)")
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
        let alive = fleet.nodes.filter(\.alive)
        return [
            ("trabajando", alive.filter { $0.state == .working }.count, Palette.sky),
            ("esperando", alive.filter { $0.state == .waitingAnswer || $0.state == .waitingMachine }.count, Palette.dawn),
            ("vivos", alive.count, Palette.sage),
        ]
    }
}

struct MachineBadge: View {
    @Environment(Fleet.self) private var fleet

    var body: some View {
        HStack(spacing: 6) {
            Circle()
                .fill(dotColor)
                .frame(width: 8, height: 8)
                .shadow(color: dotColor.opacity(0.8), radius: 4)
            Image(systemName: "desktopcomputer")
                .font(.footnote)
            if let free = fleet.machine?.memoryFreePercent {
                Text("\(free) %")
                    .font(.footnote.monospacedDigit().weight(.medium))
                    .contentTransition(.numericText())
            }
        }
        .foregroundStyle(Palette.ink.opacity(0.65))
        .animation(.smooth, value: fleet.connection)
    }

    private var dotColor: Color {
        switch fleet.connection {
        case .online: fleet.machine?.unreliable == true ? Palette.dawn : Palette.sage
        case .connecting: Palette.slate
        case .offline: Palette.rose
        }
    }
}

private struct MachineDetail: View {
    @Environment(Fleet.self) private var fleet

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Mac")
                .font(.display(.title3, weight: .bold))
            switch fleet.connection {
            case .offline(let reason):
                Label(reason, systemImage: "wifi.slash").foregroundStyle(Palette.rose)
            case .connecting:
                Label("Conectando…", systemImage: "antenna.radiowaves.left.and.right")
            case .online:
                if let machine = fleet.machine {
                    row("Memoria libre", machine.memoryFreePercent.map { "\($0) %" } ?? "—")
                    row("Memoria disponible", ByteCountFormatter.string(fromByteCount: Int64(machine.memoryAvailable), countStyle: .memory))
                    row("Carga", machine.load.prefix(3).map { $0.formatted(.number.precision(.fractionLength(1))) }.joined(separator: " · "))
                    row("Disco libre", ByteCountFormatter.string(fromByteCount: Int64(machine.diskAvailable), countStyle: .file))
                    if machine.unreliable {
                        Label("Medidas poco fiables", systemImage: "exclamationmark.triangle")
                            .foregroundStyle(Palette.dawn)
                    }
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
