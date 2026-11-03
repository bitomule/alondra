import AlondraContract
import AlondraDesign
import ComposableArchitecture
import SwiftUI

/// Settings' «Metrics» row, there only once the daemon has served the screen.
public struct MetricsRow: View {
    let store: StoreOf<MetricsFeature>

    public init(store: StoreOf<MetricsFeature>) { self.store = store }

    public var body: some View {
        if store.offered {
            Section {
                NavigationLink {
                    MetricsScreen(store: store)
                } label: {
                    Text("Metrics").foregroundStyle(Palette.ink)
                }
                .accessibilityIdentifier("settings.metrics")
            }
        }
    }
}

/// Settings › Metrics: the daemon's sections as a settings list. Rows of a section with one row on
/// screen are a choice, marked with a check; figures are label and value, in the tone the daemon
/// gives them; every other row opens its filter in place.
public struct MetricsScreen: View {
    let store: StoreOf<MetricsFeature>

    public init(store: StoreOf<MetricsFeature>) { self.store = store }

    public var body: some View {
        Form {
            if let document = store.document {
                ForEach(document.sections, id: \.id) { section in
                    Section {
                        items(section, of: document)
                    } header: {
                        if let title = section.title { Text(verbatim: title) }
                    }
                }
                if !document.notices.isEmpty {
                    Section {
                        ForEach(document.notices.indices, id: \.self) { index in
                            Label { Text(verbatim: document.notices[index].text) } icon: { Image(systemName: "info.circle") }
                                .font(.footnote)
                                .foregroundStyle(.secondary)
                        }
                    }
                    .opacity(store.loading ? 0.4 : 1)
                }
            } else {
                ProgressView().frame(maxWidth: .infinity)
            }
        }
        .formStyle(.grouped)
        .navigationTitle(Text("Metrics"))
        .toolbar {
            if store.loading {
                ToolbarItem(placement: .automatic) { ProgressView().controlSize(.small) }
            }
        }
        .animation(.smooth(duration: 0.2), value: store.loading)
        #if os(iOS)
        .scrollContentBackground(.hidden)
        .background(DawnBackground())
        .navigationBarTitleDisplayMode(.inline)
        #endif
    }

    @ViewBuilder private func items(_ section: ViewSection, of document: ViewDocument) -> some View {
        let choice = store.state.isChoice(section)
        ForEach(section.items.indices, id: \.self) { index in
            switch section.items[index] {
            case .row(let row) where choice:
                choiceRow(row, in: section, current: ViewKey(rawValue: document.key))
            case .row(let row):
                filterRow(row)
            case .facts(let facts):
                ForEach(facts.items.indices, id: \.self) { index in
                    let fact = facts.items[index]
                    LabeledContent {
                        Text(verbatim: fact.value)
                            .foregroundStyle(fact.tone.map(ListTone.tint) ?? Palette.ink)
                            .multilineTextAlignment(.trailing)
                    } label: {
                        Text(verbatim: fact.label)
                    }
                    .opacity(store.loading ? 0.4 : 1)
                }
            case .notice(let notice):
                Text(verbatim: notice.text).font(.footnote).foregroundStyle(.secondary)
            default:
                EmptyView()
            }
        }
    }

    private func choiceRow(_ row: AlondraContract.Row, in section: ViewSection, current: ViewKey) -> some View {
        let chosen = store.state.isChosen(row, in: section)
        let target = viewKey(row.link) ?? current
        return Button {
            store.send(.choose(row: row.id, key: target))
        } label: {
            HStack {
                Text(verbatim: row.title).foregroundStyle(Palette.ink)
                Spacer(minLength: 8)
                if chosen {
                    Image(systemName: "checkmark").font(.body.weight(.semibold)).foregroundStyle(Palette.dawn)
                }
            }
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(chosen ? .isSelected : [])
    }

    @ViewBuilder private func filterRow(_ row: AlondraContract.Row) -> some View {
        let label = VStack(alignment: .leading, spacing: 2) {
            Text(verbatim: row.title).foregroundStyle(Palette.ink)
            if let subtitle = row.subtitle {
                Text(verbatim: subtitle).font(.footnote).foregroundStyle(.secondary)
            }
        }
        if let key = viewKey(row.link) {
            Button {
                store.send(.choose(row: row.id, key: key))
            } label: {
                HStack {
                    label
                    Spacer(minLength: 8)
                    Image(systemName: "chevron.right").font(.footnote.weight(.semibold)).foregroundStyle(.tertiary)
                }
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
        } else {
            label
        }
    }

    private func viewKey(_ link: AlondraContract.Link?) -> ViewKey? {
        guard case .view(let key)? = link else { return nil }
        return ViewKey(rawValue: key)
    }
}
