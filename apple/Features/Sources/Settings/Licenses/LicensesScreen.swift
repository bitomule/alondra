import AlondraDesign
import SwiftUI

struct AppLicense: Identifiable, Equatable, Sendable {
    let usedBy: String
    let text: String

    var id: String { usedBy }
}

/// Settings' «Licenses» row: it opens the license of Alondra and the notices of the packages the apps link.
public struct LicensesRow: View {
    public init() {}

    public var body: some View {
        Section {
            NavigationLink {
                LicensesScreen()
            } label: {
                Text("Licenses").foregroundStyle(Palette.ink)
            }
            .accessibilityIdentifier("settings.licenses")
        }
    }
}

/// Settings › Licenses: Alondra's own license first, then every third-party license the apps distribute.
public struct LicensesScreen: View {
    public init() {}

    public var body: some View {
        Form {
            Section {
                NavigationLink {
                    LicenseTextScreen(title: Text("Alondra"), license: AppLicenses.alondra)
                } label: {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Alondra").foregroundStyle(Palette.ink)
                        Text("Functional Source License 1.1, MIT Future License")
                            .font(.footnote).foregroundStyle(.secondary)
                    }
                }
            } footer: {
                Text("The Alondra name and logo are not licensed.")
            }
            Section {
                ForEach(AppLicenses.packages) { license in
                    NavigationLink {
                        LicenseTextScreen(title: Text("License"), license: license)
                    } label: {
                        Text(verbatim: license.usedBy).foregroundStyle(Palette.ink)
                    }
                }
            } header: {
                Text("Third-party software")
            } footer: {
                Text("Open-source packages linked into the apps, each under its own license.")
            }
        }
        .formStyle(.grouped)
        .navigationTitle(Text("Licenses"))
        #if os(iOS)
        .scrollContentBackground(.hidden)
        .background(DawnBackground())
        .navigationBarTitleDisplayMode(.inline)
        #endif
    }
}

struct LicenseTextScreen: View {
    let title: Text
    let license: AppLicense

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                Text(verbatim: license.usedBy).font(.headline)
                Text(verbatim: license.text)
                    .font(.mono(.footnote))
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .padding()
        }
        .navigationTitle(title)
        #if os(iOS)
        .background(DawnBackground())
        .navigationBarTitleDisplayMode(.inline)
        #endif
    }
}
