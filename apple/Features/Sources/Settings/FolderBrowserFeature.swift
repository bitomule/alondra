import ComposableArchitecture
import Foundation

/// Walks a Mac's home folder through its daemon, so a projects folder is chosen without typing a path.
@Reducer public struct FolderBrowserFeature: Sendable {
    @ObservableState public struct State: Equatable {
        public var listing: FolderListing?
        public var loading = false
        public var error: String?
        @ObservationStateIgnored public var client: MacSettingsClient

        public init(client: MacSettingsClient, listing: FolderListing? = nil) {
            self.client = client; self.listing = listing
        }

        public static func == (lhs: Self, rhs: Self) -> Bool {
            lhs.listing == rhs.listing && lhs.loading == rhs.loading && lhs.error == rhs.error
        }
    }

    public enum Action {
        case task
        case open(String?)
        case loaded(FolderListing)
        case failed
        case use
        case delegate(Delegate)

        public enum Delegate { case chose(MacSettings.Candidate) }
    }

    private enum CancelID { case load }
    public init() {}

    public var body: some ReducerOf<Self> {
        Reduce { state, action in
            switch action {
            case .task:
                guard state.listing == nil else { return .none }
                return .send(.open(nil))
            case .open(let path):
                state.loading = true
                state.error = nil
                return .run { [client = state.client] send in
                    do { await send(.loaded(try await client.folders(path))) }
                    catch { await send(.failed) }
                }
                .cancellable(id: CancelID.load, cancelInFlight: true)
            case .loaded(let listing):
                state.loading = false
                state.listing = listing
                return .none
            case .failed:
                state.loading = false
                state.error = String(localized: "Your Mac couldn’t open this folder.")
                return .none
            case .use:
                guard let listing = state.listing else { return .none }
                return .send(.delegate(.chose(MacSettings.Candidate(path: listing.path, repos: listing.repos))))
            case .delegate:
                return .none
            }
        }
    }
}
