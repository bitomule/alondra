# AlondraContracts

Wire values shared by the Apple API client and headless callers. This module imports Foundation only. JSON field names and optionality follow the current daemon contract; presentation labels and application state belong elsewhere.

During the Bazel integration step, remove the corresponding declarations from `AlondraShared`, add `AlondraContracts` as a dependency and import it at use sites. Until that step, the old target intentionally keeps its definitions so the deployed clients remain buildable.
