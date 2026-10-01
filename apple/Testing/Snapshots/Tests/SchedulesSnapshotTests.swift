import AlondraContract
import AlondraFeatures
import AlondraFeed
import AlondraiOSUI
import ComposableArchitecture
import SwiftUI
import XCTest

/// The scheduled tasks and their editor: the list with a paused and a finished schedule, an empty
/// list, an existing schedule, a new custom one, and a form the daemon refused field by field.
@MainActor
final class SchedulesSnapshotTests: XCTestCase {
    private let lightAndDark: [SnapshotVariant] = [.light, .dark]

    private func schedules(_ document: ViewDocument) -> StoreOf<SchedulesFeature> {
        var state = SchedulesFeature.State()
        state.screen = .showing(document, key: .schedules)
        return Store(initialState: state) { SchedulesFeature() } withDependencies: {
            $0.viewsClient.observe = ViewsClient.serving(document)
        }
    }

    private func editor(_ document: ViewDocument, errors: [String: String] = [:], formError: String? = nil,
                        edit: (inout ScheduleEditorFeature.State) -> Void = { _ in }) -> StoreOf<ScheduleEditorFeature> {
        let key = ViewKey(rawValue: document.key)
        var state = ScheduleEditorFeature.State(key: key)
        state.screen = .showing(document, key: key)
        for field in state.form?.fields ?? [] { state.values[field.id] = field.value }
        state.errors = errors
        state.formError = formError
        edit(&state)
        return Store(initialState: state) { ScheduleEditorFeature() } withDependencies: {
            $0.viewsClient.observe = ViewsClient.serving(document)
        }
    }

    func test_schedules_populated() throws {
        let document = try SampleViews.document("schedules-populated")
        assertScreen { ScreenFrame(height: 420) { SchedulesView(store: schedules(document)) } }
    }

    func test_schedules_populated_spanish() throws {
        let document = try SampleViews.document("schedules-populated-es")
        assertScreen(language: .spanish) { ScreenFrame(height: 420) { SchedulesView(store: schedules(document)) } }
    }

    func test_schedules_empty() throws {
        let document = try SampleViews.document("schedules-ios")
        assertScreen(lightAndDark) { ScreenFrame(height: 420) { SchedulesView(store: schedules(document)) } }
    }

    func test_scheduleEditor_existing() throws {
        let document = try SampleViews.document("schedule-edit")
        assertScreen(inWindow: true) { ScreenFrame(height: 1400) { ScheduleEditorView(store: editor(document)) } }
    }

    func test_scheduleEditor_existing_spanish() throws {
        let document = try SampleViews.document("schedule-edit-es")
        assertScreen([.light], language: .spanish, inWindow: true) { ScreenFrame(height: 1400) { ScheduleEditorView(store: editor(document)) } }
    }

    func test_scheduleEditor_newEveryFewDays() throws {
        let document = try SampleViews.document("schedule-new-custom")
        assertScreen(lightAndDark, inWindow: true) { ScreenFrame(height: 1400) { ScheduleEditorView(store: editor(document)) } }
    }

    func test_scheduleEditor_withErrorsOnTheFieldsTheDaemonRefused() throws {
        let document = try SampleViews.document("schedule-new-custom")
        let errors = [
            "name": "Enter a name of up to 120 characters.",
            "mission": "Enter instructions of up to 20,000 characters.",
            "interval": "Choose an interval from 1 to 365.",
        ]
        assertScreen([.light], inWindow: true) { ScreenFrame(height: 1500) { ScheduleEditorView(store: editor(document, errors: errors)) } }
    }

    func test_scheduleEditor_withErrorsOnTheFieldsTheDaemonRefused_spanish() throws {
        let document = try SampleViews.document("schedule-new-custom-es")
        let errors = [
            "name": "Escribe un nombre de hasta 120 caracteres.",
            "mission": "Escribe instrucciones de hasta 20.000 caracteres.",
            "interval": "Elige un intervalo entre 1 y 365.",
        ]
        assertScreen([.light], language: .spanish, inWindow: true) {
            ScreenFrame(height: 1500) { ScheduleEditorView(store: editor(document, errors: errors)) }
        }
    }

    func test_scheduleEditor_withAFormError() throws {
        let document = try SampleViews.document("schedule-edit")
        assertScreen([.light], inWindow: true) {
            ScreenFrame(height: 1400) {
                ScheduleEditorView(store: editor(document, formError: "That schedule changed on your Mac. Open it again."))
            }
        }
    }
}
