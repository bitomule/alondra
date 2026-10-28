import AlondraDesign
import ComposableArchitecture
import SwiftUI

/// Settings › Send a crash report: «What happened?», then the report exactly as it would leave the
/// Mac, then one decision, send it or keep it on the Mac.
public struct CrashReportScreen: View {
    @Bindable var store: StoreOf<CrashReportFeature>

    public init(store: StoreOf<CrashReportFeature>) { self.store = store }

    public var body: some View {
        Form {
            switch store.stage {
            case .describing:
                describing
            case .building:
                Section {
                    HStack(spacing: 12) {
                        ProgressView()
                        Text("Collecting the report: the logs, the tasks and the nodes of the last hours…")
                    }
                } footer: {
                    Text("Nothing leaves your Mac until you say so.")
                }
            case .ready(let report), .sending(let report):
                preview(report)
            case .sent(let outcome):
                Section {
                    Label { Text(verbatim: outcome.message) } icon: { Image(systemName: "checkmark.circle.fill").foregroundStyle(Palette.sage) }
                    if let url = outcome.url {
                        SwiftUI.Link(destination: url) { Label("Open the issue", systemImage: "arrow.up.right.square") }
                    }
                }
                startOver
            case .kept(let id):
                Section {
                    Text("It stays on your Mac.")
                    Text("To send it later, run alondra report send \(id) on your Mac.")
                        .font(.footnote).foregroundStyle(.secondary).textSelection(.enabled)
                }
                startOver
            case let .failed(failure, report):
                Section {
                    Label { Self.text(failure) } icon: {
                        Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(Palette.rose)
                    }
                    if let report {
                        Text("Nothing was sent; it stays on your Mac. To send it later, run alondra report send \(report) on your Mac.")
                            .font(.footnote).foregroundStyle(.secondary).textSelection(.enabled)
                    }
                }
                .accessibilityIdentifier("crashReport.failed")
                startOver
            }
        }
        .formStyle(.grouped)
        .navigationTitle(Text("Crash report"))
        #if os(iOS)
        .scrollContentBackground(.hidden)
        .background(DawnBackground())
        .navigationBarTitleDisplayMode(.inline)
        #endif
        .sheet(item: Binding(get: { store.viewing }, set: { if $0 == nil { store.send(.closeFile) } })) { viewing in
            CrashReportFileView(viewing: viewing) { store.send(.closeFile) }
        }
    }

    private var describing: some View {
        Section {
            TextField("What happened?", text: $store.whatHappened.sending(\.describe), axis: .vertical)
                .lineLimit(3...8)
                .accessibilityIdentifier("crashReport.description")
            Button("Prepare the report") { store.send(.build) }
                .accessibilityIdentifier("crashReport.build")
        } header: {
            Text("What happened?")
        } footer: {
            Text("Optional. It goes exactly as you write it, and anyone who sees the report can read it. Your Mac then collects its logs, tasks and nodes of the last 72 hours with everything private left out, and shows you all of it before anything is sent.")
        }
    }

    @ViewBuilder private func preview(_ report: CrashReport) -> some View {
        Section {
            LabeledContent("Last \(report.hours) hours") {
                Text(verbatim: ByteCountFormatter.string(fromByteCount: Int64(report.compressedBytes), countStyle: .file))
                    .monospacedDigit()
            }
            LabeledContent("What happened?") {
                if let text = store.writtenDescription { Text(verbatim: "«\(text)»") } else { Text("Nothing written") }
            }
        } header: {
            Text("Crash report")
        } footer: {
            Text("Nothing has left your Mac yet.")
        }
        Section {
            ForEach(report.files) { file in
                if file.openable {
                    Button { store.send(.open(file.name)) } label: { fileRow(file).contentShape(Rectangle()) }
                        .buttonStyle(.plain)
                        .foregroundStyle(Palette.ink)
                } else {
                    fileRow(file).foregroundStyle(.secondary)
                }
            }
        } header: {
            Text("Files")
        } footer: {
            Text("Open any file to see it whole, exactly as it would be sent.")
        }
        if !report.leftOut.isEmpty {
            Section("Left out") {
                ForEach(report.leftOut.indices, id: \.self) { index in
                    Self.label(report.leftOut[index]).font(.footnote)
                }
            }
        }
        Section {
            if report.passed {
                Label("The final check found nothing private.", systemImage: "checkmark.shield.fill")
                    .foregroundStyle(Palette.sage)
                Text("This is exactly what is sent. Anyone with the link of its issue will be able to read it, also what you wrote in “What happened?”, which goes as you wrote it.")
            } else {
                Label("The report cannot leave your Mac; the final check found:", systemImage: "xmark.shield.fill")
                    .foregroundStyle(Palette.rose)
                ForEach(report.hits.indices, id: \.self) { index in
                    let hit = report.hits[index]
                    Text("\(hit.section), line \(hit.line): \(hit.what)").font(.footnote)
                }
            }
        }
        .accessibilityIdentifier("crashReport.check")
        decision(report)
    }

    @ViewBuilder private func decision(_ report: CrashReport) -> some View {
        if report.passed, let repo = report.repo {
            Section {
                if case .sending = store.stage {
                    HStack(spacing: 12) {
                        ProgressView()
                        Text("Sending it to \(repo)…")
                    }
                } else {
                    Button("Yes, send it") { store.send(.send) }
                        .bold()
                        .accessibilityIdentifier("crashReport.send")
                    Button("No, keep it on this Mac") { store.send(.keep) }
                        .accessibilityIdentifier("crashReport.keep")
                }
            } header: {
                Text("Send it to \(repo)? Anyone who sees its issue will be able to read it.")
            }
        } else if report.passed {
            Section {
                Text("No report repository is set up, so the report stays on your Mac.")
            }
            .accessibilityIdentifier("crashReport.noRepository")
        }
    }

    private var startOver: some View {
        Section {
            Button("New report") { store.send(.startOver) }
        }
    }

    private func fileRow(_ file: CrashReport.File) -> some View {
        HStack {
            Text(verbatim: file.name).font(.mono(.subheadline))
            Spacer()
            if let bytes = file.bytes {
                Text(verbatim: ByteCountFormatter.string(fromByteCount: Int64(bytes), countStyle: .file))
                    .monospacedDigit().foregroundStyle(.secondary)
            } else if let empty = file.empty, !empty.isEmpty {
                Text(verbatim: empty).font(.footnote).foregroundStyle(.secondary).multilineTextAlignment(.trailing)
            }
        }
    }

    static func label(_ leftOut: CrashReport.LeftOut) -> Text {
        switch leftOut {
        case let .omitted(key, count):
            let what = switch key {
            case "daemon_log_lines_without_template": Text("log lines no template of the code has")
            case "lifecycle_events_not_decoded": Text("lifecycle events it could not read")
            case "sdk_host_instances_with_unknown_names": Text("SDK hosts with a name it does not know")
            case "sdk_host_frames_not_decoded": Text("SDK host frames it could not read")
            case "sdk_host_frames_of_unknown_type": Text("SDK host frames of a type it does not know")
            default: Text(verbatim: key)
            }
            return what + Text(verbatim: ": \(count)")
        case let .trimmed(section, lines):
            return Text("cut to fit in 5 MB: the oldest \(lines) lines of \(section)")
        }
    }

    /// Why there is no report, in the screen's language.
    static func text(_ failure: CrashReportFeature.Failure) -> Text {
        switch failure {
        case .notAnswering(true): Text("Alondra’s service is not answering: open Terminal and run alondra report.")
        case .notAnswering(false): Text("Your Mac is not answering: run alondra report on it.")
        case .problem(let problem): Text(problem.title)
        case .refused(let why): Text(verbatim: why)
        case .unreadable: Text("Alondra on your Mac answered with something that is not a report.")
        }
    }
}
