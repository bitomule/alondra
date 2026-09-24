import AlondraShared
import SwiftUI

struct NewRequestSheet: View {
    let store: FleetStore
    @Environment(\.dismiss) private var dismiss
    @State private var text = ""
    @State private var attachments = Attachments()
    @State private var sending = false
    @State private var options: WorkOptionsModel?
    @FocusState private var focused: Bool

    private var trimmed: String { text.trimmingCharacters(in: .whitespacesAndNewlines) }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            VStack(alignment: .leading, spacing: 4) {
                Text("What do you want done?")
                    .font(.title3.weight(.semibold))
                Text("I’ll pass it to the orchestrator: it prepares the mission and launches the node.")
                    .foregroundStyle(.secondary)
            }
            TextEditor(text: $text)
                .font(.body)
                .scrollContentBackground(.hidden)
                .padding(8)
                .frame(minWidth: 520, minHeight: 180)
                .background(.background, in: .rect(cornerRadius: 8))
                .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(Color.primary.opacity(0.12)))
                .overlay(alignment: .topLeading) {
                    if text.isEmpty {
                        Text("For example: “Find out why Redraft is slow to open and propose a fix”")
                            .foregroundStyle(.tertiary)
                            .padding(.horizontal, 13)
                            .padding(.vertical, 8)
                            .allowsHitTesting(false)
                    }
                }
                .focused($focused)
                .modifier(DropsFiles(attachments: attachments, client: store.client))
            AttachmentStrip(attachments, client: store.client)
            if let options {
                WorkOptionsForm(model: options)
            }
            HStack {
                AttachFilesButton(attachments: attachments, client: store.client)
                Hint(text: String(localized: "⌘↩ sends · esc cancels"))
                Spacer()
                Button("Cancel", role: .cancel) { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button {
                    send()
                } label: {
                    if sending {
                        ProgressView().controlSize(.small)
                    } else {
                        Text("Send")
                    }
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.return, modifiers: .command)
                .disabled(sending || (trimmed.isEmpty && attachments.isEmpty))
            }
        }
        .padding(22)
        .onAppear { focused = true }
        .task {
            let model = WorkOptionsModel(client: store.client)
            options = model
            await model.load()
        }
    }

    private func send() {
        guard !trimmed.isEmpty || !attachments.isEmpty else { return }
        let body = (options?.options ?? WorkOptions()).body(text: trimmed)
        sending = true
        Task {
            if await store.newRequest(body, attachments: attachments) {
                dismiss()
            }
            sending = false
        }
    }
}

private struct WorkOptionsForm: View {
    @Bindable var model: WorkOptionsModel

    var body: some View {
        Grid(alignment: .leading, horizontalSpacing: 12, verticalSpacing: 8) {
            GridRow {
                Text("Project")
                    .foregroundStyle(.secondary)
                    .gridColumnAlignment(.trailing)
                Picker("Project", selection: projectBinding) {
                    Text("The orchestrator decides").tag(String?.none)
                    ForEach(model.projects, id: \.self) { Text(verbatim: $0).tag(String?.some($0)) }
                }
                .labelsHidden()
                .fixedSize()
            }
            ForEach(model.options.specs) { spec in
                GridRow {
                    Text(verbatim: spec.title)
                        .foregroundStyle(.secondary)
                    OptionControl(spec: spec, choice: choiceBinding(spec))
                }
            }
            if let error = model.options.error {
                GridRow {
                    Color.clear.gridCellUnsizedAxes([.horizontal, .vertical])
                    Label(error, systemImage: "exclamationmark.triangle")
                        .font(.caption)
                        .foregroundStyle(.orange)
                        .lineLimit(2)
                }
            }
        }
        .opacity(model.loaded ? 1 : 0.4)
        .animation(.default, value: model.loaded)
    }

    private var projectBinding: Binding<String?> {
        Binding(get: { model.options.project }, set: { project in Task { await model.choose(project: project) } })
    }

    private func choiceBinding(_ spec: OptionSpec) -> Binding<OptionChoice> {
        Binding(get: { model.options.current(spec) }, set: { model.options.set(spec, to: $0) })
    }
}

private struct OptionControl: View {
    let spec: OptionSpec
    @Binding var choice: OptionChoice

    var body: some View {
        switch spec.kind {
        case .toggle:
            Toggle(spec.title, isOn: Binding(get: { choice == .toggle(true) }, set: { choice = .toggle($0) }))
                .labelsHidden()
                .toggleStyle(.switch)
                .controlSize(.small)
        case .choice:
            let picker = Picker(spec.title, selection: $choice) {
                ForEach(spec.values) { Text(verbatim: spec.title(of: $0)).tag(OptionChoice.value($0.id)) }
            }
            .labelsHidden()
            .fixedSize()
            if spec.values.count <= 4 {
                picker.pickerStyle(.segmented)
            } else {
                picker.pickerStyle(.menu)
            }
        }
    }
}
