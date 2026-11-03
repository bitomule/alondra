import AlondraFeatures
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct SettingsDevicesTests {
    private let phone = TrustedDevice(id: "phone-1", name: "iPhone de Alice", origin: .direct)
    private let tui = TrustedDevice(id: "dev-1", name: "Terminal", origin: .mac)

    private func client(revoke: @escaping @Sendable (String) async throws -> Void,
                        devices: [TrustedDevice]) -> SettingsClient {
        var client = SettingsClient(observe: { AsyncStream { $0.finish() } }, save: { _, _ in },
            requestPermission: {}, openSystemSettings: {}, checkConnection: {}, logout: {})
        client.devices = { devices }
        client.revokeDevice = revoke
        return client
    }

    @Test func listsOnlyOtherDevicesAndRevokesAtOnce() async {
        let revoked = LockIsolated<[String]>([])
        let store = TestStore(initialState: SettingsFeature.State(service: "s", device: "mac", supportsNotifications: false)) {
            SettingsFeature()
        } withDependencies: {
            $0.serviceSettings = client(revoke: { id in revoked.withValue { $0.append(id) } }, devices: [phone, tui])
        }
        await store.send(.loadDevices)
        await store.receive(\.devicesLoaded) { $0.devices = [phone] }
        await store.send(.revokeDeviceTapped("phone-1")) { $0.confirmRevokeDeviceID = "phone-1" }
        await store.send(.confirmRevokeDevice) {
            $0.confirmRevokeDeviceID = nil
            $0.revokingDeviceID = "phone-1"
            $0.devices = []
        }
        await store.receive(\.deviceRevoked) { $0.revokingDeviceID = nil }
        #expect(revoked.value == ["phone-1"])
    }

    @Test func aFailedRevokeBringsTheDeviceBackWithASentence() async {
        struct Refused: Error {}
        var state = SettingsFeature.State(service: "s", device: "mac", supportsNotifications: false)
        state.devices = [phone]
        let store = TestStore(initialState: state) { SettingsFeature() } withDependencies: {
            $0.serviceSettings = client(revoke: { _ in throw Refused() }, devices: [phone])
        }
        await store.send(.revokeDeviceTapped("phone-1")) { $0.confirmRevokeDeviceID = "phone-1" }
        await store.send(.confirmRevokeDevice) {
            $0.confirmRevokeDeviceID = nil
            $0.revokingDeviceID = "phone-1"
            $0.devices = []
        }
        await store.receive(\.revokeFailed) {
            $0.revokingDeviceID = nil
            $0.devicesError = "Alondra couldn’t revoke this device. Try again."
        }
        await store.receive(\.loadDevices)
        await store.receive(\.devicesLoaded) { $0.devices = [phone]; $0.devicesError = nil }
    }

    @Test func onlyAMacPairedOnTheLocalNetworkCanBeForgotten() async {
        let local = MacChoice(id: UUID(), macID: "mac-1", name: "Studio", local: true)
        let relay = MacChoice(id: UUID(), macID: "mac-2", local: false)
        let forgotten = LockIsolated<[UUID]>([])
        var state = SettingsFeature.State(service: "s", device: "phone", supportsNotifications: false)
        state.macs = [local, relay]
        let store = TestStore(initialState: state) { SettingsFeature() } withDependencies: {
            var client = SettingsClient(observe: { AsyncStream { $0.finish() } }, save: { _, _ in },
                requestPermission: {}, openSystemSettings: {}, checkConnection: {}, logout: {})
            client.forgetMac = { id in forgotten.withValue { $0.append(id) } }
            $0.serviceSettings = client
        }
        await store.send(.forgetMacTapped(relay.id))
        await store.send(.forgetMacTapped(local.id)) { $0.confirmForgetMacID = local.id }
        await store.send(.confirmForgetMac) { $0.confirmForgetMacID = nil; $0.forgettingMacID = local.id }
        await store.receive(\.macForgotten) { $0.forgettingMacID = nil; $0.macs = [relay] }
        #expect(forgotten.value == [local.id])
    }
}
